//! pty の先で `leani` の bin を動かす (Python 版の `conftest.py` の `Terminal` に
//! 当たる)。
//!
//! reedline は入力行をカーソル移動で再描画するので、受け取ったバイト列から色を
//! 取り除いただけでは画面の内容にならない。そこで vt100 で端末を再現し、画面
//! そのものを確認する。カーソル位置の問い合わせ (`\x1b[6n`) にも本物の端末と同じ
//! ように応答する。応答しないと、`leani` は行編集を使わずに 1 行ずつ読む。
//!
//! `leani` が入力待ちになったかどうかは、カーソルがプロンプトの行にあるかで判定する
//! (`wait_prompt`)。`leani` が表示したものは再描画されないので、受け取った順に
//! 保存しておき (`raw`)、プロンプトが表示されたところでまとめて返す。

use std::fs;
use std::io::{Read, Write};
use std::num::NonZero;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use leani::history::BlockHistory;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use regex::bytes::Regex;

use super::strip_ansi;

pub const PROMPT: &str = "λ>";
pub const WIDE_PROMPT: &str = "λ+>";
const ROWS: u16 = 24;

const CPR: &[u8] = b"\x1b[6n";
/// 読み取りの境目で途切れたエスケープシーケンス。続きを受け取るまで画面に出さない。
static ESC_TAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[?[0-9;?]*$").expect("regex"));

const CONFIG: &str = "default = \"core\"\n\n[env.core]\nimports = []\n\n[env.wide]\nimports = [\"Lean.Elab.Frontend\"]\nprompt = \"λ+> \"\n";

/// 設定、履歴、init を置く一時ディレクトリを作る。`leani` はここから起動する。
/// lakefile の無いディレクトリから起動して、cwd に依存しないことも確認する。
pub fn state_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("config.toml"), CONFIG).expect("write config");
    dir
}

fn leani_env(dir: &Path) -> [(&'static str, PathBuf); 2] {
    [
        ("LEANI_CONFIG", dir.join("config.toml")),
        ("LEANI_INIT", dir.join("no-such-init.lean")),
    ]
}

/// 画面の文字とカーソルの位置。
type Screen = (String, (u16, u16));

/// pty の先で実行している `leani` のプロセス 1 つ。
pub struct Terminal {
    parser: vt100::Parser,
    rx: Receiver<Vec<u8>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
    /// 画面に出していない、途切れたエスケープシーケンス。
    pending: Vec<u8>,
    /// `leani` が表示したもの。`wait_prompt` が色を取り除いて返す。
    raw: Vec<u8>,
    history: PathBuf,
    closed: bool,
    /// キーを送る前の画面とカーソル。`settle` は、画面がこれと変わるまで待ってから、
    /// 出力が途切れるのを待つ。reedline は一度に 10 文字より多く受け取ると貼り付けと
    /// 見なし、続きを 100 ms 待ってから表示する。出力が途切れるのを待つだけでは、
    /// 表示の前に戻ってしまう。
    typed: Option<Screen>,
    /// 状態のディレクトリ。テストが作るファイル (評価が始まった印など) も置く。
    pub dir: tempfile::TempDir,
}

impl Terminal {
    /// 80 桁で起動し、プロンプトが出るまで待つ。
    pub fn start() -> Terminal {
        Terminal::open(80, None, "")
    }

    /// `history_name` を渡すと、`LEANI_HISTORY` をその相対パスにする。子プロセスは
    /// 状態のディレクトリから起動するので、履歴はそこに作られる。履歴のファイルには
    /// 起動の前に `seed` を書いておく。
    pub fn open(cols: u16, history_name: Option<&str>, seed: &str) -> Terminal {
        let dir = state_dir();
        let (history, history_env) = match history_name {
            Some(name) => (dir.path().join(name), PathBuf::from(name)),
            None => (dir.path().join("history"), dir.path().join("history")),
        };
        fs::write(&history, seed).expect("write history");

        let pty = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_leani"));
        cmd.cwd(dir.path());
        cmd.env("TERM", "xterm-256color");
        cmd.env("LEANI_HISTORY", &history_env);
        for (k, v) in leani_env(dir.path()) {
            cmd.env(k, v);
        }
        cmd.env_remove("LEANI_ENGINE");
        cmd.env_remove("LEANI_NO_SETUP");
        let child = pty.slave.spawn_command(cmd).expect("spawn");
        drop(pty.slave);

        let mut reader = pty.master.try_clone_reader().expect("reader");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0u8; 65536];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let mut term = Terminal {
            parser: vt100::Parser::new(
                NonZero::new(ROWS).expect("rows"),
                NonZero::new(cols).expect("cols"),
                0,
            ),
            rx,
            writer: pty.master.take_writer().expect("writer"),
            child,
            _master: pty.master,
            pending: Vec::new(),
            raw: Vec::new(),
            history,
            closed: false,
            typed: None,
            dir,
        };
        term.wait_prompt(PROMPT, 90);
        term
    }

    /// 受け取ったバイトを画面に出力する。CPR には今のカーソル位置で応答する。
    fn feed(&mut self, chunk: &[u8]) {
        self.pending.extend_from_slice(chunk);
        let cut = ESC_TAIL
            .find(&self.pending)
            .map_or(self.pending.len(), |m| m.start());
        let rest = self.pending.split_off(cut);
        let head = std::mem::replace(&mut self.pending, rest);
        let mut start = 0;
        while let Some(i) = find(&head[start..], CPR) {
            let at = start + i;
            self.show(&head[start..at]);
            // 直前までのバイトを出力したあとのカーソル位置で応答する。
            let (row, col) = self.parser.screen().cursor_position();
            self.send(&format!("\x1b[{};{}R", row + 1, col + 1));
            start = at + CPR.len();
        }
        self.show(&head[start..]);
    }

    fn show(&mut self, part: &[u8]) {
        self.parser.process(part);
        self.raw.extend_from_slice(part);
    }

    /// 出力を 1 回読む。出力が終わっていたら false を返す。
    fn pump(&mut self, budget: Duration) -> bool {
        match self.rx.recv_timeout(budget.min(Duration::from_millis(200))) {
            Ok(chunk) => {
                self.feed(&chunk);
                while let Ok(more) = self.rx.try_recv() {
                    self.feed(&more);
                }
                true
            }
            Err(RecvTimeoutError::Timeout) => true,
            Err(RecvTimeoutError::Disconnected) => false,
        }
    }

    /// 今の画面。末尾の空行は取り除く。
    pub fn screen_text(&self) -> String {
        let screen = self.parser.screen();
        let (_, cols) = screen.size();
        let rows: Vec<String> = screen
            .rows(0, cols.get())
            .map(|r| r.trim_end().to_string())
            .collect();
        rows.join("\n").trim_end().to_string()
    }

    /// カーソルの位置 (行、桁)。どちらも 0 から数える。
    pub fn cursor(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }

    pub fn cursor_line(&self) -> String {
        let screen = self.parser.screen();
        let (_, cols) = screen.size();
        let (row, _) = screen.cursor_position();
        screen
            .rows(0, cols.get())
            .nth(usize::from(row))
            .unwrap_or_default()
    }

    /// プロンプトが表示されて入力待ちになるまで待つ。sleep で待たないので速い。
    ///
    /// 保存していた出力を返して空にする。次に待つときは、それ以降の出力だけを見る。
    pub fn wait_prompt(&mut self, prompt: &str, timeout: u64) -> String {
        let end = Instant::now() + Duration::from_secs(timeout);
        loop {
            self.settle(0.2);
            if self.cursor_line().trim_start().starts_with(prompt) {
                let raw = std::mem::take(&mut self.raw);
                return strip_ansi(&String::from_utf8_lossy(&raw)).replace('\r', "");
            }
            let now = Instant::now();
            assert!(
                now <= end,
                "{prompt:?} で入力待ちにならない。画面:\n{}",
                self.screen_text()
            );
            assert!(
                self.pump(end - now),
                "{prompt:?} の前に出力が途切れた。画面:\n{}",
                self.screen_text()
            );
        }
    }

    /// 子プロセスが出力したものを読み終えるまで待つ。
    ///
    /// 画面とカーソルで判定する。再描画はカーソルを動かすだけのことがあるので、
    /// 受け取ったバイト数だけでは「まだ出力が続いている」ことを見逃す。
    pub fn settle(&mut self, quiet: f64) {
        if let Some(before) = self.typed.take() {
            let end = Instant::now() + Duration::from_secs(5);
            while self.state() == before && Instant::now() < end {
                if !self.pump(Duration::from_millis(200)) {
                    break;
                }
            }
        }
        let quiet = Duration::from_secs_f64(quiet);
        loop {
            let before = self.state();
            self.pump(quiet);
            if self.state() == before {
                return;
            }
        }
    }

    fn state(&self) -> Screen {
        (self.screen_text(), self.cursor())
    }

    pub fn type_keys(&mut self, raw: &str) {
        self.typed = Some(self.state());
        self.send(raw);
    }

    fn send(&mut self, raw: &str) {
        // 子プロセスが終了したあとは書けないが、そのときは読む側が気づく。
        let _ = self.writer.write_all(raw.as_bytes());
        let _ = self.writer.flush();
    }

    pub fn line(&mut self, text: &str) -> String {
        self.line_until(text, PROMPT, 30)
    }

    pub fn line_until(&mut self, text: &str, wait: &str, timeout: u64) -> String {
        self.type_keys(&format!("{text}\r"));
        self.wait_prompt(wait, timeout)
    }

    pub fn block(&mut self, lines: &[&str]) -> String {
        for one in lines {
            self.type_keys(&format!("{one}\r"));
            self.wait_prompt("|", 20);
        }
        self.type_keys("\r");
        self.wait_prompt(PROMPT, 40)
    }

    /// 履歴ファイルを入力した順のリストにする。`leani` と同じ実装で読み込む。
    pub fn saved_history(&self) -> Vec<String> {
        BlockHistory::new(&self.history).load()
    }

    /// `kill_it` なら SIGKILL で止め、そうでなければ `:q` で終了させる。
    pub fn close(&mut self, kill_it: bool) {
        if self.closed {
            return;
        }
        self.closed = true;
        if kill_it {
            if let Some(pid) = self.child.process_id() {
                let _ = kill(Pid::from_raw(pid.cast_signed()), Signal::SIGKILL);
            }
        } else {
            self.send(":q\r");
            self.pump(Duration::from_secs(3));
            let end = Instant::now() + Duration::from_secs(3);
            while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < end {
                self.pump(Duration::from_millis(100));
            }
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.close(false);
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// パイプ越しに `leani` へ渡して、出力 (stdout と stderr) をすべて返す。
///
/// 端末が無いときの処理を確認するためのもの。読めないバイトの扱いはここでしか
/// 確認できない。`dir` を状態のディレクトリにする。
pub fn piped(src: &[u8], dir: &Path, args: &[&str]) -> String {
    fs::write(dir.join("config.toml"), CONFIG).expect("write config");
    let (mut out, into) = std::io::pipe().expect("pipe");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_leani"));
    cmd.args(args)
        .envs(leani_env(dir))
        .env("LEANI_HISTORY", dir.join("history"))
        .env_remove("LEANI_ENGINE")
        .env_remove("LEANI_NO_SETUP")
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(into.try_clone().expect("pipe"))
        .stderr(into);
    let mut child = cmd.spawn().expect("spawn");
    // cmd が持っている書き込み側を閉じないと、読み取りが終わらない。
    drop(cmd);
    let mut stdin = child.stdin.take().expect("stdin");
    let src = src.to_vec();
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(&src);
    });
    let mut bytes = Vec::new();
    out.read_to_end(&mut bytes).expect("read");
    writer.join().expect("writer");
    child.wait().expect("wait");
    String::from_utf8_lossy(&bytes).into_owned()
}
