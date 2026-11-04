//! 端末の行編集 (副作用)。reedline に、略記の変換、Tab 補完、ブロック単位の履歴を
//! つなぐ。
//!
//! 行編集を使うのは、stdin と stdout が端末で、端末がカーソル位置の問い合わせ
//! (`\x1b[6n`) に答えるときだけにする。reedline は表示のたびにカーソルの位置を
//! 問い合わせるので、答えない端末では表示が崩れる。そのときは `cli::Stdin` で
//! 1 行ずつ読む。
//!
//! キーの割り当ては次のとおり。ほかは reedline の Emacs の既定のまま。
//!
//! - space: カーソルの手前の `\name` を記号に変えてから空白を入れる。変換の
//!   きっかけになった space を消費すると `a \to b` が `a →b` になり、記号を入力する
//!   たびに space を追加で入力することになる
//! - Tab: カーソルの手前が表にある `\name` なら、空白を入れずに記号に変える。
//!   `(\to)` のように、記号のすぐあとに文字を続けるときに使う。それ以外では
//!   候補を入力の下に並べ、共通部分まで補完する。候補が 1 つなら確定する
//!
//! reedline のキーの割り当ては決まった編集操作しか出せないので、space と Tab は
//! どちらもメニューの補完として作る。候補を 1 つだけ返すと、quick completion が
//! すぐに確定する。

use std::borrow::Cow;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use leani_core::abbrev::expand_abbrev;
use leani_core::pure::Style;
use leani_read::places::Ctx;
use reedline::{
    ColumnarMenu, Completer, CompletionResult, EditCommand, Emacs, FileBackedHistory, Highlighter,
    History, HistoryItem, HistoryItemId, HistorySessionId, KeyCode, KeyModifiers, MenuBuilder,
    Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus, Reedline,
    ReedlineEvent, ReedlineMenu, SearchQuery, Signal, Span, StyledText, Suggestion,
    default_emacs_keybindings,
};

use crate::cli::{Line, LineSource};
use crate::repl::Repl;

/// reedline で 1 行ずつ読む。
pub struct Terminal {
    editor: Reedline,
    style: Style,
}

impl Terminal {
    /// 行編集を使えるなら作る。使えなければ None を返し、呼び出し元は `cli::Stdin`
    /// で読む。
    ///
    /// 補完は `repl` をロックしてエンジンに問い合わせる。`cli::run` は `read_line` の
    /// あいだロックを持たないので、デッドロックしない。
    pub fn open(ctx: &Ctx, repl: &Arc<Mutex<Repl>>) -> Option<Terminal> {
        if !ctx.tty || !ctx.stdin_tty || !answers_cursor_query() {
            return None;
        }
        let entries = lock(repl)
            .history()
            .map_or_else(|| Arc::new(Mutex::new(Vec::new())), |h| h.entries());
        let editor = Reedline::create()
            .with_history(Box::new(Recall::new(entries)))
            .with_highlighter(Box::new(Plain))
            .with_quick_completions(true)
            .with_partial_completions(true)
            .with_menu(ReedlineMenu::WithCompleter {
                menu: Box::new(ColumnarMenu::default().with_name(SPACE).with_marker("")),
                completer: Box::new(SpaceAbbrev),
            })
            .with_menu(ReedlineMenu::WithCompleter {
                menu: Box::new(ColumnarMenu::default().with_name(TAB).with_marker("")),
                completer: Box::new(TabComplete {
                    repl: Arc::clone(repl),
                }),
            })
            .with_edit_mode(Box::new(Emacs::new(keys())))
            .with_ansi_colors(ctx.color)
            .use_bracketed_paste(true)
            .use_kitty_keyboard_enhancement(false);
        Some(Terminal {
            editor,
            style: Style { color: ctx.color },
        })
    }
}

impl LineSource for Terminal {
    fn read_line(&mut self, prompt: &str) -> Line {
        let prompt = LeanPrompt {
            left: prompt.to_string(),
            more: self.style.dim(" | "),
        };
        match self.editor.read_line(&prompt) {
            Ok(Signal::Success(line)) => Line::Text(line),
            Ok(Signal::CtrlC) => Line::Interrupt,
            Ok(_) | Err(_) => Line::End,
        }
    }
}

/// 端末がカーソル位置の問い合わせに答えるか。答えなければ 2 秒待って false を返す。
fn answers_cursor_query() -> bool {
    crossterm::cursor::position().is_ok()
}

/// space と Tab のメニューの名前。
const SPACE: &str = "abbrev";
const TAB: &str = "completion";

fn keys() -> reedline::Keybindings {
    let mut keys = default_emacs_keybindings();
    keys.add_binding(
        KeyModifiers::NONE,
        KeyCode::Char(' '),
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu(SPACE.to_string()),
            ReedlineEvent::Edit(vec![EditCommand::InsertChar(' ')]),
        ]),
    );
    keys.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu(TAB.to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );
    keys
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn suggestion(value: String, start: usize, end: usize) -> Suggestion {
    Suggestion {
        value,
        span: Span { start, end },
        append_whitespace: false,
        ..Suggestion::default()
    }
}

// ------------------------------------------------------------------ 補完

/// space のメニューの補完。候補は常に 1 つで、略記なら記号と空白、そうでなければ
/// 空白だけを入れる。
struct SpaceAbbrev;

impl Completer for SpaceAbbrev {
    fn complete(&mut self, line: &str, pos: usize) -> CompletionResult {
        let one = match expand_abbrev(&line[..pos]) {
            Some((sym, back)) => suggestion(format!("{sym} "), pos - back, pos),
            None => suggestion(" ".to_string(), pos, pos),
        };
        CompletionResult::fresh(vec![one])
    }
}

/// Tab のメニューの補完。表にある `\name` なら記号 1 つを返す。それ以外は `Repl` に
/// 候補を問い合わせる。
///
/// `Repl::complete` も `\name` の途中なら記号の候補を返すが、`\to` は `\top` などの
/// 接頭辞でもあるので、候補が 1 つにならない。表にあるキーと一致したら、ほかの候補を
/// 出さずに変換する。
struct TabComplete {
    repl: Arc<Mutex<Repl>>,
}

impl Completer for TabComplete {
    fn complete(&mut self, line: &str, pos: usize) -> CompletionResult {
        let head = &line[..pos];
        if let Some((sym, back)) = expand_abbrev(head) {
            return CompletionResult::fresh(vec![suggestion(sym.to_string(), pos - back, pos)]);
        }
        // panic で端末が raw モードのまま残らないように、補完の中の panic は
        // 候補が無いことにする。ロックは catch_unwind の外で取る。
        let mut repl = lock(&self.repl);
        let found = catch_unwind(AssertUnwindSafe(|| repl.complete(head))).unwrap_or_default();
        CompletionResult::fresh(
            found
                .into_iter()
                .map(|c| suggestion(c.text, c.start, pos))
                .collect::<Vec<_>>(),
        )
    }
}

// ------------------------------------------------------------------ 履歴

/// reedline に渡す履歴。reedline が `read_line` のたびに保存しようとする行は捨て、
/// `Repl` が `BlockHistory::record` で足した件だけを持つ。
///
/// 件の一覧は `BlockHistory` と共有している。reedline が履歴を読むたびに、前回から
/// 増えた分を reedline の形に写す。
struct Recall {
    inner: Mutex<Copied>,
}

struct Copied {
    store: FileBackedHistory,
    entries: Arc<Mutex<Vec<String>>>,
    /// `entries` のうち `store` に写した件数。
    seen: usize,
}

impl Recall {
    fn new(entries: Arc<Mutex<Vec<String>>>) -> Recall {
        // 件数の上限は設けない (prompt_toolkit もファイルの全件を読む)。
        let store = FileBackedHistory::new(usize::MAX / 2).expect("容量は上限より小さい");
        Recall {
            inner: Mutex::new(Copied {
                store,
                entries,
                seen: 0,
            }),
        }
    }

    /// 増えた分を写してから返す。
    fn synced(&self) -> MutexGuard<'_, Copied> {
        let mut c = lock(&self.inner);
        let all = lock(&c.entries).clone();
        for e in all.iter().skip(c.seen) {
            let _ = c.store.save(HistoryItem::from_command_line(e));
        }
        c.seen = all.len();
        c
    }
}

impl History for Recall {
    fn save(&mut self, h: HistoryItem) -> reedline::Result<HistoryItem> {
        Ok(h)
    }

    fn load(&self, id: HistoryItemId) -> reedline::Result<HistoryItem> {
        self.synced().store.load(id)
    }

    fn count(&self, query: SearchQuery) -> reedline::Result<i64> {
        self.synced().store.count(query)
    }

    fn search(&self, query: SearchQuery) -> reedline::Result<Vec<HistoryItem>> {
        self.synced().store.search(query)
    }

    fn update(
        &mut self,
        _id: HistoryItemId,
        _updater: &dyn Fn(HistoryItem) -> HistoryItem,
    ) -> reedline::Result<()> {
        Ok(())
    }

    fn clear(&mut self) -> reedline::Result<()> {
        Ok(())
    }

    fn delete(&mut self, _h: HistoryItemId) -> reedline::Result<()> {
        Ok(())
    }

    fn sync(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn session(&self) -> Option<HistorySessionId> {
        None
    }
}

// ------------------------------------------------------------------ 表示

/// プロンプト。色は `Repl::prompt` が付けたものを使い、reedline の色は付けない。
struct LeanPrompt {
    left: String,
    /// 履歴から呼び出した複数行の入力の、2 行目以降の頭に出す。
    more: String,
}

impl Prompt for LeanPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.left)
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.more)
    }

    fn render_prompt_history_search_indicator(&self, s: PromptHistorySearch) -> Cow<'_, str> {
        Cow::Owned(match s.status {
            PromptHistorySearchStatus::Passing => format!("(履歴を検索: {}) ", s.term),
            PromptHistorySearchStatus::Failing => format!("(見つからない: {}) ", s.term),
        })
    }

    fn get_prompt_color(&self) -> reedline::Color {
        reedline::Color::Default
    }

    fn get_prompt_multiline_color(&self) -> reedline::Color {
        reedline::Color::Default
    }

    fn get_indicator_color(&self) -> reedline::Color {
        reedline::Color::Default
    }

    fn get_prompt_right_color(&self) -> reedline::Color {
        reedline::Color::Default
    }
}

/// 入力に色を付けない。reedline の既定は入力を白で表示するので、背景の明るい端末
/// では読めない。
struct Plain;

impl Highlighter for Plain {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let mut text = StyledText::new();
        text.push((Default::default(), line.to_string()));
        text
    }
}
