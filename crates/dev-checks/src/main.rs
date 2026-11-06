//! leani の開発の手順から外れたことを、Claude Code の hook で AI に知らせる。
//! サブコマンドと決まりは lib.rs の先頭に書いてある。

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use dev_checks::{Event, Gap, Notice, Origin, Pair};
use serde_json::json;

fn main() -> ExitCode {
    // hook はリポジトリの直下で動かす (.claude/hooks/dev-checks.sh)。
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
                ["test-texts"] => dev_checks::test_texts(&root).and_then(|lines| {
                    lines.iter().for_each(|line| println!("{line}"));
                    save(&root, "test-texts.txt", &lines)
                }),
                ["test-texts-check"] => check_answers(&root),
                ["test-texts-lint"] => {
                    lint(&root).and_then(|script| lint_test_texts(&root, &script))
                }
                ["lint-ja", files @ ..] if !files.is_empty() => {
                    lint(&root).and_then(|script| lint_ja(&root, &script, files))
                }
                ["base"] => {
                    println!("{}", dev_checks::BASE);
                    Ok(())
                }
                ["can-start", name] => dev_checks::start(&root, name),
                ["can-finish"] => dev_checks::finish(&root, None),
                ["can-finish", "--head", head] => dev_checks::finish(&root, Some(head)),
                ["after-finish", old, new] => {
                    dev_checks::after_finish(&root, old, new).map(|text| {
                        text.iter().for_each(|t| println!("{t}"));
                    })
                }
                ["langs-diff"] => {
                    dev_checks::langs_diff(&root).map(|found| show_langs_diff(&found))
                }
                ["glossary-check"] => {
                    dev_checks::check_glossary(&root).and_then(|gaps| show_gaps(&gaps))
                }
                _ => Err(USAGE.to_string()),
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

const USAGE: &str = "使い方: dev-checks (post-edit | stop | status | test-texts | base \
     | can-start <名前> | can-finish [--head <commit>] | after-finish <前の commit> <後の commit> \
     | langs-diff | glossary-check | test-texts-check | test-texts-lint | lint-ja <ファイル…>)";

/// 行を [`dev_checks::STATE`] の name に書く。
fn save(root: &Path, name: &str, lines: &[String]) -> Result<(), String> {
    let state = root.join(dev_checks::STATE);
    fs::create_dir_all(&state).map_err(|e| e.to_string())?;
    let path = state.join(name);
    fs::write(
        &path,
        lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
    )
    .map_err(|e| format!("{}: {e}", path.display()))
}

/// AI の答え (test-texts.json) を確かめる。ng の文があれば、一覧を出して 1 で終わる。
fn check_answers(root: &Path) -> Result<(), String> {
    let path = root.join(dev_checks::STATE).join("test-texts.json");
    let answer = fs::read_to_string(&path).map_err(|e| {
        format!(
            "{}: {e}。just test-texts の文ごとの判断を、スキル leani-test-text の形で書く",
            path.display()
        )
    })?;
    let current = dev_checks::test_texts(root)?;
    let ng = dev_checks::check_test_texts(&current, &answer)?;
    if ng.is_empty() {
        println!("{} 個の文が、すべて ok", current.len());
        return Ok(());
    }
    ng.iter().for_each(|n| println!("{n}"));
    Err(format!(
        "ng の文が {} 個ある。直してから just test-texts からやり直す",
        ng.len()
    ))
}

/// 使う yomiyasu の lint のパスと版を出す。
fn lint(root: &Path) -> Result<std::path::PathBuf, String> {
    let (script, version) = dev_checks::find_lint(root)?;
    println!("yomiyasu の lint: {} (版: {version})", script.display());
    Ok(script)
}

/// ファイルごとに lint を動かし、指摘を `ファイル:行: [規則] 文` の形で出す。
fn lint_ja(root: &Path, script: &Path, files: &[&str]) -> Result<(), String> {
    let found = files
        .iter()
        .map(|f| {
            dev_checks::lint_file(root, script, Path::new(f)).map(|found| {
                found
                    .iter()
                    .map(|x| format!("{f}:{}: [{}] {}", x.line, x.rule, x.message))
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Result<Vec<_>, String>>()?
        .concat();
    found.iter().for_each(|f| println!("{f}"));
    println!("指摘: {} 個", found.len());
    Ok(())
}

/// 足した ja: の文を test-texts.ja.txt に 1 行ずつ書いて lint にかけ、指摘の行を元の場所に戻して出す。
fn lint_test_texts(root: &Path, script: &Path) -> Result<(), String> {
    let (places, sentences): (Vec<String>, Vec<String>) = dev_checks::test_texts(root)?
        .iter()
        .filter_map(|line| {
            let (place, text) = line.split_once(": ")?;
            let ja = text.strip_prefix("ja: ")?;
            Some((place.to_string(), ja.to_string()))
        })
        .unzip();
    save(root, "test-texts.ja.txt", &sentences)?;
    let file = root.join(dev_checks::STATE).join("test-texts.ja.txt");
    let found = dev_checks::lint_file(root, script, &file)?;
    found.iter().for_each(|x| {
        let place = usize::try_from(x.line)
            .ok()
            .and_then(|n| places.get(n.wrapping_sub(1)))
            .map_or("test-texts.ja.txt", String::as_str);
        println!("{place}: [{}] {}", x.rule, x.message);
    });
    println!("ja: の文 {} 個、指摘: {} 個", sentences.len(), found.len());
    Ok(())
}

fn show_langs_diff(found: &[Pair]) {
    if found.is_empty() {
        println!("片方だけを変えた文書は無い");
        return;
    }
    found.iter().for_each(|p| {
        let origin = match &p.origin {
            Origin::Tree => "作業ツリー".to_string(),
            Origin::NewFile => "追跡していない新しいファイル。全体を読んで訳す".to_string(),
            Origin::Commit(text) => format!("commit: {text}"),
        };
        println!("## {} → {} ({origin})", p.changed, p.other);
        print!("{}", p.diff);
        println!();
    });
}

/// 誤りがあれば 1 で終わる。候補だけなら 0 で終わる。
fn show_gaps(gaps: &[Gap]) -> Result<(), String> {
    gaps.iter().for_each(|g| match g {
        Gap::Missing { ja, en, term } => println!(
            "誤り: {ja} に足した行に「{term}」があるのに、{en} に glossary.toml の英語の語が無い"
        ),
        Gap::Candidate { ja, en, term } => println!(
            "候補: {ja} に足した行に「{term}」があるのに、{en} の足した行に glossary.toml の英語の語が無い。\
             語だけを直した場合や、代名詞にした場合は、そのままでよい"
        ),
    });
    match gaps.iter().any(|g| matches!(g, Gap::Missing { .. })) {
        true => Err("glossary.toml の英語の語が無い英語の文書がある".to_string()),
        false => {
            if gaps.is_empty() {
                println!("glossary.toml の語は、英語の文書の足した行にある");
            }
            Ok(())
        }
    }
}

fn status(root: &Path) -> Result<(), String> {
    let tree = dev_checks::tree(root)?;
    let found = dev_checks::all(root, &tree);
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
    let given = dev_checks::input(&raw, root)?;
    let state = root.join(dev_checks::STATE);
    fs::create_dir_all(&state).map_err(|e| e.to_string())?;
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let input = serde_json::from_str::<serde_json::Value>(&raw).unwrap_or_default();
    // git を動かせないときは、作業を止めないように何も返さず、理由をログにだけ残す。
    // 人は just status で理由を読める。
    let tree = match dev_checks::tree(root) {
        Ok(tree) => tree,
        Err(why) => {
            let log = json!({"time": time, "input": input, "error": why});
            return append(&state.join("log.jsonl"), &format!("{log}\n"));
        }
    };
    // SPEC の確かめは時間がかかるので、AI が終わるときだけ行う。
    let found: Vec<Notice> = match given.event {
        Event::Stop { active: false } => dev_checks::all(root, &tree),
        _ => dev_checks::notices(&tree),
    };
    let seen_file = state.join(format!("seen-{}", safe(&given.session)));
    let seen: Vec<String> = fs::read_to_string(&seen_file)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    let (out, keys) = dev_checks::reply(&given.event, &found, &seen);
    append(
        &seen_file,
        &keys.iter().map(|k| format!("{k}\n")).collect::<String>(),
    )?;
    let log = json!({
        "time": time,
        "input": input,
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
