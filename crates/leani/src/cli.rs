//! エントリーポイント (副作用)。
//!
//! 引数を読んで `Repl` を起動する。`process::exit` はここでは呼ばず、終了コードと
//! エラーのメッセージを `Result` で bin (`main.rs`) に返す。`Err(メッセージ)` を
//! 受け取った bin は、`leani: メッセージ` を stderr に出して終了コード 1 で終わる。
//!
//! 入力を読むループ (`run`) も `Repl` の外のここに置く。`Repl` は入力を持たず、
//! 1 行ずつ `feed_line` で受け取るだけにする。入力の元は `LineSource` で差し替え、
//! 端末、パイプ、テストの列を同じループで動かす。
//!
//! stdout には `print!` で書かない。出力先が閉じていると (`leani | head` の head が
//! 終了した) `print!` は panic するので、書けなくても続ける `emit` で書く。

use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

use leani_core::pure::{Style, engine_dir, indent};
use leani_core::types::EngineFault;
use leani_read::args::parse_args;
use leani_read::config::{EnvConfig, load_config, problem, resolve};
use leani_read::places::{Ctx, LOOGLE_TIMEOUT};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};

use crate::boot::{ensure_engine, guess_toolchain, toolchain};
use crate::repl::{HELP, Repl};
use crate::search::Loogle;
use crate::show::Console;

/// `-h` の表示。設定ファイルと init ファイルとエンジンの置き場所は `Ctx` から取る。
pub fn usage(ctx: &Ctx) -> String {
    format!(
        "\
leani [オプション] [file.lean]

  -e, --env <name>     設定した環境で起動する
  -i, --import <Mod>   import するモジュールを指定する (繰り返せる)。指定すると、
                       プロジェクトの lean_lib や -e の環境の imports の代わりに使う
  -p, --project <dir>  Lake プロジェクトを指定する
      --setup          エンジンを用意して終了する (通常は起動時に自動で用意する)
  -V, --version        環境、Lean のバージョン、ファイルの場所を表示する
  -h, --help           このヘルプを表示する

環境は {config} に書く。設定が無ければ、カレントディレクトリの
lakefile から Lake プロジェクトと lean_lib を推測する。Lake プロジェクトの外では
Lean 本体だけで起動する。init ファイルは {init}。

エンジン (leanprover-community/repl) は初回だけ git clone と lake build で用意し、
使う Lean のバージョンごとに {cache} の下に置く。
自分でビルドしたエンジンを使う場合は、設定の env.<名前>.engine か engine、
または LEANI_ENGINE で指定する (前に書いたものほど優先)。

",
        config = ctx.config,
        init = ctx.init,
        cache = ctx.engine_cache,
    )
}

/// `-V` の表示。エンジンが未ビルドでも、設定が無くても、場所だけは表示する。
///
/// Lean のバージョンを調べているあいだに Ctrl-C が押されたら `Interrupted` を返す。
pub fn version_text(ctx: &Ctx, cfg: &EnvConfig) -> Result<String, EngineFault> {
    let tc = guess_toolchain(ctx, cfg)?;
    let engine = tc
        .as_deref()
        .and_then(|tc| engine_dir(cfg.engine.as_deref(), tc, &ctx.engine_cache).ok())
        .unwrap_or_else(|| format!("{}/<バージョン>", ctx.engine_cache));
    let ready = if has_repl(&engine) {
        ""
    } else {
        " (未ビルド)"
    };
    let config = if Path::new(&ctx.config).is_file() {
        ""
    } else {
        " (無し)"
    };
    Ok(format!(
        "leani\n  \
         環境:     {cfg}\n  \
         プロジェクト: {}\n  \
         Lean:     {}\n  \
         エンジン: {engine}{ready}\n  \
         設定:     {}{config}\n  \
         init:     {}\n  \
         履歴:     {}\n",
        cfg.project.as_deref().unwrap_or("(無し)"),
        tc.as_deref().unwrap_or("(不明)"),
        ctx.config,
        ctx.init,
        ctx.hist,
    ))
}

fn has_repl(engine: &str) -> bool {
    Path::new(&format!("{engine}/.lake/build/bin/repl")).is_file()
}

/// 引数を読んで起動する。戻り値は終了コード。`Err` はメッセージを表示して終了コード 1。
///
/// 入力の元は、`Repl` を起動したあとに `source` で作る。端末の Tab 補完が `Repl` を
/// 使うためである。起動しなかったとき (`-h`、`-V`、`--setup`、起動の失敗) は作らない。
///
/// Ctrl-C で起動を中断したときは、改行を出して 130 で終わる。`--setup` と `-V` も同じ。
pub fn main(
    ctx: &Ctx,
    argv: &[String],
    source: impl FnOnce(&Arc<Mutex<Repl>>) -> Box<dyn LineSource>,
) -> Result<i32, String> {
    let args = parse_args(argv)?;
    if args.help {
        emit(&format!("{}{HELP}", usage(ctx)));
        return Ok(0);
    }

    let table = load_config(&ctx.config).map_err(|e| e.0)?;
    let cfg = resolve(
        ctx,
        args.name.as_deref(),
        args.project.as_deref(),
        &args.imports,
        &table,
    )
    .map_err(|e| e.0)?;

    if args.version {
        return match version_text(ctx, &cfg) {
            Ok(text) => {
                emit(&text);
                Ok(0)
            }
            Err(e) => give_up(&e),
        };
    }
    if let Some(why) = problem(ctx, &cfg) {
        return Err(why);
    }
    if args.setup {
        return match setup(ctx, &cfg) {
            Ok(()) => Ok(0),
            Err(e) => give_up(&e),
        };
    }

    let started = Repl::new(
        ctx,
        cfg,
        args.preload.as_deref(),
        Box::new(Console::new(ctx)),
        Box::new(Loogle::new(ctx, LOOGLE_TIMEOUT)),
    );
    let repl = match started {
        Ok(repl) => repl,
        Err(e) => return give_up(&e),
    };
    let repl = Arc::new(Mutex::new(repl));
    let mut src = source(&repl);
    let code = run(&repl, src.as_mut());
    lock(&repl).close();
    code
}

/// `--setup`。用意済みならそう表示するだけで、エンジンを作り直さない。
fn setup(ctx: &Ctx, cfg: &EnvConfig) -> Result<(), EngineFault> {
    let tc = toolchain(ctx, cfg)?;
    let path = engine_dir(cfg.engine.as_deref(), &tc, &ctx.engine_cache)?;
    if has_repl(&path) {
        let style = Style { color: ctx.color };
        emit(&format!("{}\n", style.dim(&format!("用意済み: {path}"))));
        return Ok(());
    }
    ensure_engine(ctx, &mut io::stdout(), cfg.engine.as_deref(), &tc, true)?;
    Ok(())
}

/// 起動しなかったときの終了コード。Ctrl-C で中断したときは、改行を出して 130 を返す。
fn give_up(e: &EngineFault) -> Result<i32, String> {
    match e {
        EngineFault::Interrupted => {
            emit("\n");
            Ok(130)
        }
        _ => Err(start_failure(e)),
    }
}

/// stdout に書く。書けなくても (出力先が閉じていても) 何もしない。
fn emit(text: &str) {
    let mut stdout = io::stdout().lock();
    let _ = stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush());
}

/// 起動できなかった理由。用意の失敗 (`Setup`) は理由がそのまま読めるので、そのまま
/// 表示する。プロセスの失敗は、何が起きたかの前に「起動しなかった」ことを書く。
fn start_failure(e: &EngineFault) -> String {
    match e {
        EngineFault::Setup(why) => why.clone(),
        _ => match e.to_string() {
            why if why.is_empty() => "エンジンが起動しなかった".to_string(),
            why => format!("エンジンが起動しなかった\n{}", indent(&why, "  ")),
        },
    }
}

// ------------------------------------------------------------------ 入力のループ

/// 1 行の入力の元。端末、パイプ、テストの列。
pub trait LineSource {
    /// プロンプトを表示して 1 行読む。
    fn read_line(&mut self, prompt: &str) -> Line;
}

/// `read_line` の結果。
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    /// 読めた 1 行。末尾の改行は含まない。
    Text(String),
    /// 入力の終わり (Ctrl-D、パイプの EOF)。
    End,
    /// 行の編集中の Ctrl-C。
    Interrupt,
}

/// 入力を 1 行ずつ `Repl` に渡す。戻り値は終了コード。
///
/// 入力の終わり (パイプの EOF、端末の Ctrl-D) では、入力途中のブロックがあれば
/// 空行を入力したときと同じく確定して、ループを続ける。無ければ終わる。パイプは次も
/// 入力の終わりを返すので、そこで終わる。端末では、もう一度 Ctrl-D を押すと終わる。
///
/// 出力先が閉じたら、残りの入力は実行せずに終わる。結果を読む人がいないためである。
///
/// `:env` で元の環境にも戻れなくなったときは、環境が無いので続けず、理由を `Err` で
/// 返す。`read_line` のあいだはロックを持たない。端末の
/// Tab 補完が同じ `Mutex` をロックしてエンジンに問い合わせるためである。
pub fn run(repl: &Mutex<Repl>, src: &mut dyn LineSource) -> Result<i32, String> {
    loop {
        let prompt = {
            let mut r = lock(repl);
            if r.out().closed() {
                return Ok(0);
            }
            // 評価のあとに押された Ctrl-C は feed_line のあとで消している。
            // 補完や表示の途中で押されたものが残っていても、次の入力には関係ない。
            r.take_interrupt();
            r.prompt()
        };
        match src.read_line(&prompt) {
            Line::End => {
                let mut r = lock(repl);
                r.out().write("");
                if !r.pending() {
                    return Ok(0);
                }
                r.end_block();
            }
            Line::Interrupt => lock(repl).cancel(),
            Line::Text(line) => {
                let mut r = lock(repl);
                if r.feed_line(&line) {
                    return match r.env_lost() {
                        Some(why) => Err(why.to_string()),
                        None => Ok(0),
                    };
                }
                // Engine の中で押された Ctrl-C は Interrupted になって再起動まで済んで
                // いる。その外 (整形、補完) で押されたものはフラグに残るので、ここで
                // 入力中のブロックを捨てる。
                if r.take_interrupt() {
                    r.cancel();
                }
            }
        }
    }
}

/// ロックを取る。`feed_line` は panic を捕まえるので、毒になることはまず無いが、
/// なっていても中身はそのまま使える。
fn lock(repl: &Mutex<Repl>) -> std::sync::MutexGuard<'_, Repl> {
    repl.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ------------------------------------------------------------------ 行単位の入力

/// stdin から 1 行ずつ読む。パイプと、端末だが行編集 (`term`) を使えないときの入力。
///
/// UTF-8 として読めないバイトは置換文字にして Lean に渡し、構文エラーとして普通に
/// 報告させる。読めないバイトが 1 つあるだけで
/// 後続の行まで失わないように。
///
/// 読むあいだは 0.25 秒ごとに Ctrl-C のフラグを確かめ、立っていたら行の編集中の
/// Ctrl-C と同じ扱いにする。`BufReader` を使わず fd から直接読むのは、`poll` で
/// 待つのが fd だからである。`BufReader` にたまった分は `poll` に見えない。
pub struct Stdin {
    buf: Vec<u8>,
    eof: bool,
    interrupted: Arc<AtomicBool>,
}

impl Stdin {
    pub fn new(ctx: &Ctx) -> Stdin {
        Stdin {
            buf: Vec::new(),
            eof: false,
            interrupted: ctx.interrupted.clone(),
        }
    }

    /// ためた分から 1 行を取り出す。改行が無ければ None。
    fn take_line(&mut self) -> Option<String> {
        let end = self.buf.iter().position(|&b| b == b'\n')?;
        let line: Vec<u8> = self.buf.drain(..=end).collect();
        Some(decode(&line[..end]))
    }

    /// stdin を少し待って、読めた分をためる。EOF なら `eof` にする。
    fn fill(&mut self) -> Line {
        let stdin = io::stdin();
        let mut fds = [PollFd::new(stdin.as_fd(), PollFlags::POLLIN)];
        match poll(&mut fds, PollTimeout::from(POLL_MS)) {
            Ok(0) | Err(nix::errno::Errno::EINTR) => {
                if self.interrupted.swap(false, SeqCst) {
                    return Line::Interrupt;
                }
            }
            Ok(_) => {
                let mut chunk = [0u8; 1 << 12];
                match nix::unistd::read(stdin.as_raw_fd(), &mut chunk) {
                    Ok(0) | Err(_) => self.eof = true,
                    Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                }
            }
            Err(_) => self.eof = true,
        }
        Line::End
    }
}

/// Ctrl-C のフラグを確かめる間隔 (ミリ秒)。
const POLL_MS: u16 = 250;

/// 行のバイト列を文字列にする。末尾の `\r` は改行の一部として落とす。
fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

impl LineSource for Stdin {
    fn read_line(&mut self, prompt: &str) -> Line {
        emit(prompt);
        loop {
            if let Some(line) = self.take_line() {
                return Line::Text(line);
            }
            if self.eof {
                if self.buf.is_empty() {
                    return Line::End;
                }
                return Line::Text(decode(&std::mem::take(&mut self.buf)));
            }
            if self.fill() == Line::Interrupt {
                return Line::Interrupt;
            }
        }
    }
}
