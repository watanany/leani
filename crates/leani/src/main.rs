//! `leani` コマンド。環境変数と引数を読み、`cli::main` の結果を終了コードにする。
//!
//! プロセス全体の状態に触るのはここだけにする。SIGINT のハンドラは `Ctx` の
//! フラグを立てるだけで、確かめる箇所 (エンジンの応答待ち、入力の読み取り) が
//! フラグを読んで消す。

#![allow(clippy::print_stderr)]

use std::process::ExitCode;

use leani::cli::{self, Stdin};
use leani_read::places::Ctx;

fn main() -> ExitCode {
    let ctx = Ctx::from_process();
    if let Err(e) =
        signal_hook::flag::register(signal_hook::consts::SIGINT, ctx.interrupted.clone())
    {
        eprintln!("leani: SIGINT のハンドラを登録できない: {e}");
    }
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut stdin = Stdin::new(&ctx);
    match cli::main(&ctx, &argv, &mut stdin) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(why) => {
            eprintln!("leani: {why}");
            ExitCode::from(1)
        }
    }
}
