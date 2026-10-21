//! `SPEC.rust.md` を生成する。使い方は lib.rs の先頭に書いてある。

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::path::Path;
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
        ["--missing", path] => spec::missing(&root, Path::new(path)).map(|left| {
            left.iter().for_each(|line| println!("{line}"));
            println!("移していないテスト {} 件", left.len());
        }),
        _ => Err("使い方: spec [--check | --missing SPEC.md]".to_string()),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("{why}");
            ExitCode::FAILURE
        }
    }
}
