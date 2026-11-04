//! 表示を出す (副作用)。
//!
//! 組み立てた表示を `Output` に出す。組み立ての処理は `leani_core::pure` にある。
//! 端末の幅と高さもここで読む (`:abbrev` の表と `:loogle` の結果を端末の幅に合わせる)。

use std::io::Write;
use std::process::{Command, Stdio};

use leani_core::pure::{
    Style, caret_len, caret_pad, has_error, indent, messages, panic_line, sorries, span, text,
};
use leani_core::types::{Json, Output};
use leani_read::places::Ctx;

/// 端末用の `Output`。`leani | tee log` で log に残すため、エラーも stdout に出す。
///
/// stdout に書けなくなったら (`leani | head` の head が終了した)、それを覚えて、
/// 以後は何も書かない。`print!` は書けないと panic するので使わない。
pub struct Console {
    /// stdout が端末か。端末でなければ pager を使わない。
    tty: bool,
    /// `:abbrev` の一覧を表示する pager のコマンド。`sh -c` で実行する。
    pager: String,
    /// `sh` に渡す PATH とカレントディレクトリ。プロセスの状態は `Ctx` だけから読む。
    path: String,
    cwd: String,
    /// stdout に書けなかったか。
    closed: bool,
}

impl Console {
    pub fn new(ctx: &Ctx) -> Console {
        Console {
            tty: ctx.tty,
            pager: ctx.pager.clone(),
            path: ctx.path.clone(),
            cwd: ctx.cwd.clone(),
            closed: false,
        }
    }

    /// `sh -c` で実行するコマンド。
    fn sh(&self, cmd: &str) -> Command {
        let mut sh = Command::new("sh");
        sh.args(["-c", cmd])
            .env("PATH", &self.path)
            .current_dir(&self.cwd);
        sh
    }
}

impl Output for Console {
    fn put(&mut self, text: &str) {
        if self.closed {
            return;
        }
        let mut stdout = std::io::stdout().lock();
        self.closed = stdout
            .write_all(text.as_bytes())
            .and_then(|()| stdout.flush())
            .is_err();
    }

    fn fail(&mut self, text: &str) {
        self.write(text);
    }

    fn closed(&self) -> bool {
        self.closed
    }

    fn shell(&mut self, cmd: &str) {
        // 出力を受け取らず、端末につないだまま実行する。vim のような対話するコマンドも
        // 使えるように。
        let _ = self.sh(cmd).status();
    }

    /// 画面に収まらないときだけ pager を使う。`leani | tee log` のように端末で
    /// なければ、そのまま出力する。pager を起動できなければ、そのまま出力する。
    fn page(&mut self, text: &str) {
        let tall = text.matches('\n').count() + 1 >= rows();
        if !(self.tty && tall) {
            self.write(text);
            return;
        }
        let spawned = self.sh(&self.pager).stdin(Stdio::piped()).spawn();
        let Ok(mut child) = spawned else {
            self.write(text);
            return;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = writeln!(stdin, "{text}");
        }
        let _ = child.wait();
    }
}

/// 端末の幅。端末でなければ 80。
pub fn columns() -> usize {
    terminal_size::terminal_size().map_or(80, |(w, _)| usize::from(w.0))
}

/// 端末の高さ。端末でなければ 24。
fn rows() -> usize {
    terminal_size::terminal_size().map_or(24, |(_, h)| usize::from(h.0))
}

/// エンジンが PANIC を出力したら、普通の結果として扱わずに警告を表示する。
///
/// 利用者が入力したもの (式、do の読み直し、宣言、タクティク) の応答だけを調べる。
/// `:l`、init、replay の応答は調べない。利用者が意図して panic! を書いたファイルも
/// あるので、そのファイルの読み込みを失敗にしないため。
pub fn panic_check(out: &mut dyn Output, style: Style, resp: &Json) -> bool {
    match panic_line(resp) {
        None => false,
        Some(line) => {
            out.fail(&style.red("エンジンが PANIC した。:restart で再起動したほうが安全"));
            out.write(&style.dim(&line));
            true
        }
    }
}

/// メッセージの severity に合わせた色。
fn paint(style: Style, sev: &str, s: &str) -> String {
    match sev {
        "error" => style.red(s),
        "warning" => style.yellow(s),
        _ => s.to_string(),
    }
}

/// メッセージを GHCi 風に出す。位置があれば該当行とキャレットを添える。
pub fn render(
    out: &mut dyn Output,
    style: Style,
    resp: &Json,
    src: &str,
    line_off: i64,
    col_off: i64,
) {
    let lines: Vec<&str> = src.lines().collect();

    for m in messages(resp) {
        let sev = text(m, "severity").unwrap_or("information");
        let data = text(m, "data").unwrap_or("").trim_end();
        let at = match sev {
            "error" | "warning" => span(m, &lines, line_off, col_off),
            _ => None,
        };
        let shown = match at {
            None => paint(style, sev, data),
            Some((ln, col, width)) => {
                let head = format!("{ln}:{}", col + 1);
                let line = lines.get(ln - 1).copied().unwrap_or("");
                let marks = "^".repeat(caret_len(line, col, width));
                let caret =
                    " ".repeat(head.len() + 2) + &caret_pad(line, col) + &paint(style, sev, &marks);
                format!(
                    "{}  {line}\n{caret}\n{}",
                    style.dim(&head),
                    paint(style, sev, &indent(data, "  "))
                )
            }
        };
        // error のメッセージは fail に出す。位置の行とキャレットも含めて 1 回で出す。
        if sev == "error" {
            out.fail(&shown);
        } else {
            out.write(&shown);
        }
    }

    if let Some(note) = text(resp, "message").map(str::trim)
        && !note.is_empty()
    {
        // repl がリクエスト全体を拒否した (env や proofState が無い)。messages は
        // 空なので、ここで表示しないと画面に何も表示されない。
        out.fail(&style.red(note));
        out.write(&style.dim("  leani が持っている環境がエンジンに無い。:restart で再起動できる"));
    }

    if has_error(resp) {
        // エラーになった宣言は環境に追加されていない。その sorry のゴールを表示しても
        // 証明を書けないので、エラーの後ろに役に立たないゴールが並ぶだけになる。
        return;
    }

    for (i, sy) in sorries(resp).iter().enumerate() {
        let state = sy
            .get("proofState")
            .map(ToString::to_string)
            .unwrap_or_else(|| "null".to_string());
        out.write(&style.green(&format!("sorry {} [proofState {state}]", i + 1)));
        let goal = text(sy, "goal").unwrap_or("").trim_end();
        out.write(&indent(goal, "  "));
    }
}
