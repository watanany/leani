//! `SPEC.ja.md` と `SPEC.en.md` を生成する。使い方は lib.rs の先頭に書いてある。

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;

use spec::Lang;

fn main() -> ExitCode {
    let root = spec::root();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let done = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => Lang::ALL.iter().try_for_each(|&lang| {
            spec::generate(&root, lang).and_then(|text| {
                std::fs::write(root.join(lang.out()), text).map_err(|e| e.to_string())
            })
        }),
        ["--check"] => {
            let bad: Vec<String> = Lang::ALL
                .iter()
                .filter_map(|&lang| spec::check(&root, lang).err())
                .collect();
            match bad.is_empty() {
                true => Ok(()),
                false => Err(bad.join("\n")),
            }
        }
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
