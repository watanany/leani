//! 行編集の crate を決めるための spike。reedline で次を確かめる。
//!
//! - space: カーソルの手前の `\name` を記号に変えてから空白を入れる
//! - Tab: 表にある `\name` を記号に変える。それ以外は候補のメニューを出し、1 つなら確定する
//! - 補完の関数が `read_line` の中から `Arc<Mutex<…>>` をロックできる
//! - 独自の `History` で prompt_toolkit の形式を読み、ブロックを 1 件として記録する
//! - 色付きのプロンプト、継続行のプロンプト、Ctrl-C、Ctrl-D
//!
//! `SPIKE_HISTORY` に履歴ファイルのパスを渡す。`spike_pty` がこの例を端末で動かす。
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::borrow::Cow;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::sync::{Arc, Mutex};

use reedline::{
    ColumnarMenu, Completer, CompletionResult, EditCommand, Emacs, FileBackedHistory, History,
    HistoryItem, HistoryItemId, HistorySessionId, KeyCode, KeyModifiers, MenuBuilder, Prompt,
    PromptEditMode, PromptHistorySearch, Reedline, ReedlineEvent, ReedlineMenu, SearchQuery,
    Signal, Span, Suggestion, default_emacs_keybindings,
};

const ABBREV: &[(&str, &str)] = &[
    ("to", "→"),
    ("all", "∀"),
    ("ex", "∃"),
    ("alpha", "α"),
    ("and", "∧"),
    ("(", "("),
    ("<", "⟨"),
    ("le", "≤"),
];

const NAMES: &[&str] = &["Nat.add_comm", "Nat.add_zero", "Nat.succ", "List.map"];

/// `read_line` の外にある状態。補完の関数がロックして読む。
struct Session {
    queries: usize,
}

/// カーソルの手前の `\name` と、その開始位置 (byte)。
fn abbrev_head(line: &str, pos: usize) -> Option<(usize, &str)> {
    let head = &line[..pos];
    head.rfind('\\').map(|i| (i, &head[i + 1..]))
}

fn lookup(name: &str) -> Option<&'static str> {
    ABBREV.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

fn suggestion(value: String, start: usize, end: usize) -> Suggestion {
    Suggestion {
        value,
        span: Span { start, end },
        append_whitespace: false,
        ..Suggestion::default()
    }
}

/// space の menu の completer。候補は常に 1 つなので、quick completion で確定する。
struct SpaceAbbrev;

impl Completer for SpaceAbbrev {
    fn complete(&mut self, line: &str, pos: usize) -> CompletionResult {
        let one = match abbrev_head(line, pos).and_then(|(i, n)| lookup(n).map(|s| (i, s))) {
            Some((i, sym)) => suggestion(format!("{sym} "), i, pos),
            None => suggestion(" ".to_string(), pos, pos),
        };
        CompletionResult::fresh(vec![one])
    }
}

/// Tab の completer。表にある `\name` なら記号 1 つ、そうでなければ名前の候補。
struct TabComplete {
    session: Arc<Mutex<Session>>,
}

impl Completer for TabComplete {
    fn complete(&mut self, line: &str, pos: usize) -> CompletionResult {
        if let Some((i, sym)) = abbrev_head(line, pos).and_then(|(i, n)| lookup(n).map(|s| (i, s)))
        {
            return CompletionResult::fresh(vec![suggestion(sym.to_string(), i, pos)]);
        }
        let mut session = self.session.lock().expect("lock");
        session.queries += 1;
        let start = line[..pos]
            .rfind(|c: char| c.is_whitespace() || c == '(')
            .map_or(0, |i| i + 1);
        let word = &line[start..pos];
        let found: Vec<Suggestion> = NAMES
            .iter()
            .filter(|n| n.starts_with(word))
            .map(|n| suggestion((*n).to_string(), start, pos))
            .collect();
        CompletionResult::fresh(found)
    }
}

/// reedline が保存しようとする行は捨て、`record` で記録したブロックだけを持つ履歴。
struct BlockHistory {
    inner: Arc<Mutex<FileBackedHistory>>,
}

impl History for BlockHistory {
    fn save(&mut self, h: HistoryItem) -> reedline::Result<HistoryItem> {
        Ok(h)
    }
    fn load(&self, id: HistoryItemId) -> reedline::Result<HistoryItem> {
        self.inner.lock().expect("lock").load(id)
    }
    fn count(&self, query: SearchQuery) -> reedline::Result<i64> {
        self.inner.lock().expect("lock").count(query)
    }
    fn search(&self, query: SearchQuery) -> reedline::Result<Vec<HistoryItem>> {
        self.inner.lock().expect("lock").search(query)
    }
    fn update(
        &mut self,
        id: HistoryItemId,
        updater: &dyn Fn(HistoryItem) -> HistoryItem,
    ) -> reedline::Result<()> {
        self.inner.lock().expect("lock").update(id, updater)
    }
    fn clear(&mut self) -> reedline::Result<()> {
        self.inner.lock().expect("lock").clear()
    }
    fn delete(&mut self, h: HistoryItemId) -> reedline::Result<()> {
        self.inner.lock().expect("lock").delete(h)
    }
    fn sync(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn session(&self) -> Option<HistorySessionId> {
        None
    }
}

/// prompt_toolkit の FileHistory の形式 (`# 時刻` の行と `+` で始まる行) を読む。
fn parse_history(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines() {
        match line.strip_prefix('+') {
            Some(rest) => lines.push(rest),
            None if !lines.is_empty() => out.push(std::mem::take(&mut lines).join("\n")),
            None => {}
        }
    }
    if !lines.is_empty() {
        out.push(lines.join("\n"));
    }
    out
}

fn append_history(path: &str, block: &str) -> std::io::Result<()> {
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    let body: String = block.lines().map(|l| format!("+{l}\n")).collect();
    write!(f, "\n# spike\n{body}")
}

struct LeanPrompt {
    text: &'static str,
}

impl Prompt for LeanPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(self.text)
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed(" | ")
    }
    fn render_prompt_history_search_indicator(&self, _s: PromptHistorySearch) -> Cow<'_, str> {
        Cow::Borrowed("(検索) ")
    }
}

fn incomplete(line: &str) -> bool {
    let t = line.trim_end();
    t.ends_with(":=") || t.ends_with(" by") || t.ends_with(" where")
}

fn main() -> std::io::Result<()> {
    let path = std::env::var("SPIKE_HISTORY").unwrap_or_else(|_| "spike_history".into());
    let inner = Arc::new(Mutex::new(FileBackedHistory::new(1000).expect("history")));
    for block in parse_history(&fs::read_to_string(&path).unwrap_or_default()) {
        let _ = inner
            .lock()
            .expect("lock")
            .save(HistoryItem::from_command_line(block));
    }
    let session = Arc::new(Mutex::new(Session { queries: 0 }));

    let mut keys = default_emacs_keybindings();
    keys.add_binding(
        KeyModifiers::NONE,
        KeyCode::Char(' '),
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("abbrev".into()),
            ReedlineEvent::Edit(vec![EditCommand::InsertChar(' ')]),
        ]),
    );
    keys.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion".into()),
            ReedlineEvent::MenuNext,
        ]),
    );

    let mut editor = Reedline::create()
        .with_history(Box::new(BlockHistory {
            inner: Arc::clone(&inner),
        }))
        .with_quick_completions(true)
        .with_menu(ReedlineMenu::WithCompleter {
            menu: Box::new(ColumnarMenu::default().with_name("abbrev").with_marker("")),
            completer: Box::new(SpaceAbbrev),
        })
        .with_menu(ReedlineMenu::WithCompleter {
            menu: Box::new(
                ColumnarMenu::default()
                    .with_name("completion")
                    .with_marker(""),
            ),
            completer: Box::new(TabComplete {
                session: Arc::clone(&session),
            }),
        })
        .with_edit_mode(Box::new(Emacs::new(keys)));

    let main_prompt = LeanPrompt {
        text: "\x1b[1;34mλ>\x1b[0m ",
    };
    let cont_prompt = LeanPrompt { text: " | " };
    let mut block: Vec<String> = Vec::new();
    loop {
        let prompt = if block.is_empty() {
            &main_prompt
        } else {
            &cont_prompt
        };
        match editor.read_line(prompt)? {
            Signal::Success(line) => {
                let done = block.is_empty() && !incomplete(&line) || line.trim().is_empty();
                block.push(line);
                if !done {
                    continue;
                }
                let src = block.join("\n").trim_end().to_string();
                block.clear();
                println!(
                    "got: {src:?} queries={}",
                    session.lock().expect("lock").queries
                );
                if !src.is_empty() {
                    let _ = inner
                        .lock()
                        .expect("lock")
                        .save(HistoryItem::from_command_line(&src));
                    append_history(&path, &src)?;
                }
            }
            Signal::CtrlC => {
                block.clear();
                println!("^C");
            }
            Signal::CtrlD => {
                println!("bye");
                return Ok(());
            }
            other => println!("signal: {other:?}"),
        }
    }
}
