//! `SPEC.md` を生成する。使い方は lib.rs の先頭に書いてある。

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;

fn main() -> ExitCode {
    let root = spec::root();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let done = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => spec::generate(&root)
            .and_then(|text| std::fs::write(root.join(spec::OUT), text).map_err(|e| e.to_string())),
        ["--check"] => spec::check(&root),
        _ => Err("使い方: spec [--check]".to_string()),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("{why}");
            ExitCode::FAILURE
        }
    }
}
