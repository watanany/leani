//! `spike` を擬似端末で動かし、キー入力のあとの画面を表示する。
//! 端末のテストの土台 (`portable-pty` + `vt100`) を確かめるための spike。
//!
//!     cargo build -p leani --example spike && cargo run -p leani --example spike_pty
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::{Read, Write};
use std::num::NonZero;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const ROWS: u16 = 24;
const COLS: u16 = 80;

/// 擬似端末の画面。カーソル位置の問い合わせ (`ESC [ 6 n`) には vt100 のカーソル位置を返す。
struct Term {
    parser: vt100::Parser,
    rx: Receiver<Vec<u8>>,
    writer: Box<dyn Write + Send>,
    tail: Vec<u8>,
}

impl Term {
    /// 出力が `quiet` のあいだ途切れるまで読む。
    fn settle(&mut self, quiet: Duration) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match self.rx.recv_timeout(quiet) {
                Ok(bytes) => self.feed(&bytes),
                Err(_) => return,
            }
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        // 問い合わせが読み取りの境目で分かれる場合に備え、前回の末尾とつなげて探す
        let mut joined = std::mem::take(&mut self.tail);
        joined.extend_from_slice(bytes);
        let query = b"\x1b[6n";
        if std::env::var_os("NO_ANSWER").is_some() {
            self.parser.process(bytes);
            return;
        }
        let mut start = 0;
        while let Some(i) = find(&joined[start..], query) {
            let at = start + i;
            self.parser.process(&joined[start..at]);
            let (row, col) = self.parser.screen().cursor_position();
            let answer = format!("\x1b[{};{}R", row + 1, col + 1);
            self.writer.write_all(answer.as_bytes()).expect("write");
            start = at + query.len();
        }
        let rest = &joined[start..];
        let keep = (1..query.len())
            .rev()
            .find(|&n| rest.len() >= n && rest.ends_with(&query[..n]))
            .unwrap_or(0);
        self.parser.process(&rest[..rest.len() - keep]);
        self.tail = rest[rest.len() - keep..].to_vec();
    }

    fn send(&mut self, keys: &str) {
        self.writer.write_all(keys.as_bytes()).expect("write");
        self.writer.flush().expect("flush");
        self.settle(Duration::from_millis(300));
    }

    fn show(&self, title: &str) {
        println!("--- {title}");
        let screen = self.parser.screen();
        for line in screen.contents().lines() {
            if !line.trim().is_empty() {
                println!("|{line}");
            }
        }
        let (r, c) = screen.cursor_position();
        println!("(cursor {r},{c})");
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn main() {
    let dir = std::env::temp_dir().join(format!("leani-spike-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let history = dir.join("history");
    std::fs::write(
        &history,
        "\n# 2024-01-01 00:00:00.000000\n+theorem old : 1 = 1 := by\n+  rfl\n",
    )
    .expect("write history");

    let pty = native_pty_system()
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let exe = std::env::current_exe().expect("exe");
    let mut cmd = CommandBuilder::new(exe.with_file_name("spike"));
    cmd.env("SPIKE_HISTORY", &history);
    cmd.env("TERM", "xterm-256color");
    let mut child = pty.slave.spawn_command(cmd).expect("spawn");
    drop(pty.slave);

    let mut reader = pty.master.try_clone_reader().expect("reader");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut term = Term {
        parser: vt100::Parser::new(
            NonZero::new(ROWS).expect("rows"),
            NonZero::new(COLS).expect("cols"),
            0,
        ),
        rx,
        writer: pty.master.take_writer().expect("writer"),
        tail: Vec::new(),
    };
    term.settle(Duration::from_millis(500));
    term.show("起動");

    let steps: &[(&str, &str)] = &[
        ("#check (\\to ", "space で (\\to を展開"),
        ("\x15", "Ctrl-U"),
        ("\\all x, \\ex y, x \\le y ", "速く打った複数の略記"),
        ("\x15\\( \\< ", "\\( と \\<"),
        ("\x15\\alpha\t", "Tab で略記を展開"),
        ("\x15#check Nat.add_c\t", "Tab で候補が 1 つ"),
        ("\x15#check Nat.\t", "Tab で候補のメニュー"),
        ("\t\r", "メニューで選んで Enter"),
        ("\r", "Enter で送る"),
        ("theorem t : 1 = 1 := by\r", "継続行"),
        ("  rfl\r", "継続行 2"),
        ("\r", "空行でブロックを送る"),
        ("\x1b[A", "↑ で直前のブロック"),
        ("\x1b[A", "もう一度 ↑ でファイルの履歴"),
        ("\x03", "Ctrl-C"),
        ("\x1b[200~pasted \\to text\x1b[201~", "bracketed paste"),
        ("\x03", "Ctrl-C"),
        ("1\r2\r", "続けて打った 2 行"),
        ("\x04", "Ctrl-D"),
    ];
    for (keys, title) in steps {
        term.send(keys);
        term.show(title);
    }

    let _ = child.kill();
    println!("--- 履歴ファイル");
    print!("{}", std::fs::read_to_string(&history).unwrap_or_default());
}
