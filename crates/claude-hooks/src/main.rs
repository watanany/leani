//! leani の開発の手順から外れたことを、Claude Code の hook で AI に知らせる。
//! サブコマンドと決まりは lib.rs の先頭に書いてある。

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use claude_hooks::{Event, Notice};
use serde_json::json;

fn main() -> ExitCode {
    // hook はリポジトリの直下で動かす (.claude/hooks/claude-hooks.sh)。
    let done = std::env::current_dir()
        .map_err(|e| e.to_string())
        .and_then(|root| {
            let args: Vec<String> = std::env::args().skip(1).collect();
            match args
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice()
            {
                ["post-edit"] | ["stop"] => hook(&root),
                ["status"] => status(&root),
                ["texts"] => claude_hooks::texts(&root).map(|lines| {
                    lines.iter().for_each(|line| println!("{line}"));
                }),
                _ => Err("使い方: claude-hooks (post-edit|stop|status|texts)".to_string()),
            }
        });
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("{why}");
            ExitCode::FAILURE
        }
    }
}

fn status(root: &Path) -> Result<(), String> {
    let tree = claude_hooks::tree(root)?;
    let found = claude_hooks::all(root, &tree);
    println!("枝: {}", tree.branch);
    if found.is_empty() {
        println!("開発の手順から外れていない");
        return Ok(());
    }
    found.iter().for_each(|n| println!("- {}", n.text()));
    Ok(())
}

/// hook の入力を stdin から読み、応答を stdout に出す。
fn hook(root: &Path) -> Result<(), String> {
    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .map_err(|e| e.to_string())?;
    let given = claude_hooks::input(&raw, root)?;
    let tree = claude_hooks::tree(root)?;
    // SPEC の確かめは時間がかかるので、AI が終わるときだけ行う。
    let found: Vec<Notice> = match given.event {
        Event::Stop { active: false } => claude_hooks::all(root, &tree),
        _ => claude_hooks::notices(&tree),
    };
    let state = root.join(claude_hooks::STATE);
    fs::create_dir_all(&state).map_err(|e| e.to_string())?;
    let seen_file = state.join(format!("seen-{}", safe(&given.session)));
    let seen: Vec<String> = fs::read_to_string(&seen_file)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    let (out, keys) = claude_hooks::reply(&given.event, &found, &seen);
    append(
        &seen_file,
        &keys.iter().map(|k| format!("{k}\n")).collect::<String>(),
    )?;
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let log = json!({
        "time": time,
        "input": serde_json::from_str::<serde_json::Value>(&raw).unwrap_or_default(),
        "notices": found.iter().map(Notice::key).collect::<Vec<_>>(),
        "reply": out,
    });
    append(&state.join("log.jsonl"), &format!("{log}\n"))?;
    if let Some(out) = out {
        println!("{out}");
    }
    Ok(())
}

/// セッションの名前を、ファイルの名前に使える文字だけにする。
fn safe(session: &str) -> String {
    session
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

fn append(path: &Path, text: &str) -> Result<(), String> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))
}
