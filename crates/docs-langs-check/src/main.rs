//! 英語の文書と日本語の文書が対になっているかを確かめる。決まりは lib.rs の先頭に書いてある。

#![allow(clippy::print_stderr)]

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let done = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--check"] => docs_langs_check::check(&docs_langs_check::root()),
        _ => Err("使い方: docs-langs-check --check".to_string()),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("{why}");
            ExitCode::FAILURE
        }
    }
}
