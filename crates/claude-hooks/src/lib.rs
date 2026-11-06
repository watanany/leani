//! leani の開発の手順から外れたことを、Claude Code の hook で AI に知らせる。
//!
//! 知らせるのは次の 4 つである。どれも AI を止めるためではなく、手順に戻ってもらうための
//! ものである。理由があれば、AI はそのまま続けてよい (DECISIONS.ja.md の
//! 「不採用: hook で開発の手順を強制する」)。
//!
//! - `main-rs` か `main` の上で、直接ファイルを変えている
//! - テストを変えずに、コードを変えている
//! - 対の文書の片方だけを変えている
//! - `SPEC.ja.md` か `SPEC.en.md` がテストと合っていない (Stop の hook と `status` だけで確かめる)
//!
//! | サブコマンド | 使う所                                                                          |
//! |--------------|---------------------------------------------------------------------------------|
//! | `post-edit`  | PostToolUse の hook (Edit と Write)。変えたファイルに関係する知らせを返す       |
//! | `stop`       | Stop の hook。知らせがあれば、AI が終わる前に 1 回だけ止めて、知らせを読ませる |
//! | `status`     | `just status` とスキル。今の知らせを、人が読む形で出す                          |
//! | `texts`      | スキル leani-test-text。`main-rs` から足したテストの文を出す                    |
//!
//! 1 つのセッションの中では、同じ知らせを hook ごとに 1 回だけ返す。hook の入力と応答は
//! [`STATE`] の `log.jsonl` に残す。

use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

/// 枝を切る元の枝。Rust 版は `main-rs` に集めている。
pub const BASE: &str = "main-rs";

/// この上で直接ファイルを変えると知らせる枝。
pub const BASES: &[&str] = &["main-rs", "main"];

/// hook のログと、セッションごとに返した知らせを置くディレクトリ (リポジトリの直下から)。
pub const STATE: &str = "target/claude-hooks";

/// 作業ツリーの状態。
#[derive(Debug, PartialEq)]
pub struct Tree {
    /// 今の枝の名前。
    pub branch: String,
    /// [`BASE`] から変えたファイル (commit したものと、していないものと、untracked のもの)。
    pub changed: Vec<String>,
}

/// 開発の手順から外れていることの知らせ。
#[derive(Debug, PartialEq)]
pub enum Notice {
    OnBase { branch: String },
    CodeWithoutTest,
    OneSided { changed: String, other: String },
    SpecStale(String),
}

impl Notice {
    /// 同じ知らせを 2 回返さないために使う名前。
    pub fn key(&self) -> String {
        match self {
            Notice::OnBase { .. } => "on-base".to_string(),
            Notice::CodeWithoutTest => "code-without-test".to_string(),
            Notice::OneSided { changed, .. } => format!("one-sided:{changed}"),
            Notice::SpecStale(_) => "spec-stale".to_string(),
        }
    }

    /// AI に読ませる文。
    pub fn text(&self) -> String {
        match self {
            Notice::OnBase { branch } => format!(
                "{branch} の上で直接ファイルを変えている。DEVELOPMENT.ja.md の「作業の流れ」のとおり、\
                 `git switch -c <枝の名前>` で枝を切ってから作業する。"
            ),
            Notice::CodeWithoutTest => "コードを変えたが、テスト (crates/leani/tests/ と tests/) を変えていない。\
                 動作を変えるときは、ストーリー、テスト、実装の順に書く (DEVELOPMENT.ja.md の「作業の流れ」)。\
                 動作を変えないリファクタリングなら、このまま続けてよい。"
                .to_string(),
            Notice::OneSided { changed, other } => format!(
                "{changed} を変えたが、対になる {other} を変えていない。\
                 同じ commit で {other} にも同じ変更を入れる (スキル leani-docs-sync)。"
            ),
            Notice::SpecStale(why) => format!(
                "SPEC.ja.md か SPEC.en.md がテストと合っていない。`cargo run -p spec` で生成し直す ({})。",
                why.lines().collect::<Vec<_>>().join(" / ")
            ),
        }
    }

    /// path のファイルを変えたときに返す知らせか。
    pub fn about(&self, path: &str) -> bool {
        match self {
            Notice::OnBase { .. } => counts(path),
            Notice::CodeWithoutTest => is_code(path),
            Notice::OneSided { changed, .. } => path == changed,
            Notice::SpecStale(_) => false,
        }
    }
}

/// 知らせの対象にするファイルか。`.` で始まるファイル (`.envrc` や `.claude/`) と、
/// リポジトリの外のファイルは対象にしない。
fn counts(path: &str) -> bool {
    !(path.is_empty() || path.starts_with('.') || path.starts_with('/'))
}

fn is_code(path: &str) -> bool {
    (path.starts_with("crates/") && path.contains("/src/")) || path.starts_with("src/leani/")
}

fn is_test(path: &str) -> bool {
    path.starts_with("crates/leani/tests/") || path.starts_with("tests/")
}

/// 作業ツリーから作る知らせ。`SPEC.ja.md` と `SPEC.en.md` は確かめない ([`all`] で確かめる)。
pub fn notices(tree: &Tree) -> Vec<Notice> {
    let counted: Vec<&String> = tree.changed.iter().filter(|path| counts(path)).collect();
    let on_base = match BASES.contains(&tree.branch.as_str()) && !counted.is_empty() {
        true => vec![Notice::OnBase {
            branch: tree.branch.clone(),
        }],
        false => vec![],
    };
    let untested = match counted.iter().any(|path| is_code(path))
        && !counted.iter().any(|path| is_test(path))
    {
        true => vec![Notice::CodeWithoutTest],
        false => vec![],
    };
    let one_sided = counted
        .iter()
        .filter(|path| !docs_pair::exempt(path))
        .filter_map(|path| docs_pair::partner(path).map(|other| (path, other)))
        .filter(|(_, other)| !tree.changed.contains(other))
        .map(|(path, other)| Notice::OneSided {
            changed: path.to_string(),
            other,
        });
    on_base
        .into_iter()
        .chain(untested)
        .chain(one_sided)
        .collect()
}

/// hook が受け取った出来事。
#[derive(Debug, PartialEq)]
pub enum Event {
    /// Edit か Write でファイルを変えた。path はリポジトリの直下からの名前で、
    /// リポジトリの外のファイルは絶対パスのまま。
    PostEdit { path: String },
    /// AI が終わろうとしている。active は、Stop の hook が止めたあとにもう一度動いたとき true。
    Stop { active: bool },
}

/// hook の入力。
#[derive(Debug, PartialEq)]
pub struct Input {
    pub session: String,
    pub event: Event,
}

/// Claude Code が hook に渡す JSON を読む。root はリポジトリの直下。
pub fn input(raw: &str, root: &Path) -> Result<Input, String> {
    let value: Value = serde_json::from_str(raw).map_err(|e| format!("hook の入力: {e}"))?;
    let event = match value["hook_event_name"].as_str() {
        Some("PostToolUse") => Ok(Event::PostEdit {
            path: relative(
                value["tool_input"]["file_path"].as_str().unwrap_or(""),
                root,
            ),
        }),
        Some("Stop" | "SubagentStop") => Ok(Event::Stop {
            active: value["stop_hook_active"].as_bool().unwrap_or(false),
        }),
        other => Err(format!("扱わない hook: {other:?}")),
    }?;
    Ok(Input {
        session: value["session_id"].as_str().unwrap_or("").to_string(),
        event,
    })
}

fn relative(path: &str, root: &Path) -> String {
    match Path::new(path).strip_prefix(root) {
        Ok(rest) => rest.to_string_lossy().into_owned(),
        Err(_) => path.to_string(),
    }
}

const POST_HEAD: &str = "leani の開発の手順から外れているかもしれない (crates/claude-hooks)。\
     直すか、理由があればそのまま続ける。";

const STOP_HEAD: &str = "終わる前の確認。leani の開発の手順から外れているかもしれない (crates/claude-hooks)。\
     直すか、直さない理由をユーザーへの返事に書く。この知らせは、このセッションでは繰り返さない。";

/// hook の応答の JSON と、新しく返した知らせの名前。seen は、このセッションで既に返した知らせの名前。
/// 返す知らせが無ければ `(None, [])` を返す。
pub fn reply(event: &Event, found: &[Notice], seen: &[String]) -> (Option<String>, Vec<String>) {
    let (prefix, picked): (&str, Vec<&Notice>) = match event {
        Event::PostEdit { path } => ("post", found.iter().filter(|n| n.about(path)).collect()),
        Event::Stop { active: false } => ("stop", found.iter().collect()),
        Event::Stop { active: true } => ("stop", vec![]),
    };
    let fresh: Vec<(&Notice, String)> = picked
        .into_iter()
        .map(|n| (n, format!("{prefix}:{}", n.key())))
        .filter(|(_, key)| !seen.contains(key))
        .collect();
    let keys: Vec<String> = fresh.iter().map(|(_, key)| key.clone()).collect();
    let body = |head: &str| {
        std::iter::once(head.to_string())
            .chain(fresh.iter().map(|(n, _)| format!("- {}", n.text())))
            .collect::<Vec<_>>()
            .join("\n")
    };
    match (fresh.is_empty(), event) {
        (true, _) => (None, vec![]),
        (false, Event::PostEdit { .. }) => (
            Some(
                json!({"hookSpecificOutput": {
                    "hookEventName": "PostToolUse",
                    "additionalContext": body(POST_HEAD),
                }})
                .to_string(),
            ),
            keys,
        ),
        (false, Event::Stop { .. }) => (
            Some(json!({"decision": "block", "reason": body(STOP_HEAD)}).to_string()),
            keys,
        ),
    }
}

/// `git diff -U0` の出力から、足した `ja:` と `en:` の行を `ファイル:行: 文` の形で返す。
pub fn added_texts(diff: &str) -> Vec<String> {
    diff.lines()
        .fold(
            (String::new(), 0usize, Vec::new()),
            |(file, line, out), text| match (text.strip_prefix("+++ "), text.strip_prefix('+')) {
                (Some(name), _) => (
                    name.strip_prefix("b/").unwrap_or(name).to_string(),
                    line,
                    out,
                ),
                (None, Some(added)) => {
                    let sentence = sentence(added);
                    let out = match sentence {
                        Some(s) => out
                            .into_iter()
                            .chain([format!("{file}:{line}: {s}")])
                            .collect(),
                        None => out,
                    };
                    (file, line + 1, out)
                }
                (None, None) => (file, hunk_start(text).unwrap_or(line), out),
            },
        )
        .2
}

/// 足した行が `ja:` か `en:` の行なら、その文を `ja: ...` の形で返す。
fn sentence(added: &str) -> Option<String> {
    let trimmed = added.trim_start();
    let rest = trimmed
        .strip_prefix("///")
        .or_else(|| trimmed.strip_prefix("//!"))
        .unwrap_or(trimmed)
        .trim_start();
    match rest.starts_with("ja:") || rest.starts_with("en:") {
        true => Some(rest.trim_end().to_string()),
        false => None,
    }
}

/// `@@ -a,b +c,d @@` の c。
fn hunk_start(text: &str) -> Option<usize> {
    let after = text.strip_prefix("@@ ")?.split(" +").nth(1)?;
    after.split([',', ' ']).next().and_then(|n| n.parse().ok())
}

/// hook は sandbox の外で動くので、リポジトリの `.git/config` に書いたコマンド (fsmonitor、
/// 外部の diff、textconv) を git に実行させない。
fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    match out.status.success() {
        true => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        false => Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        )),
    }
}

/// [`BASE`] と今の commit が分かれた commit。[`BASE`] が無ければ `HEAD`。
pub fn base(root: &Path) -> String {
    git(root, &["merge-base", "HEAD", BASE])
        .map(|id| id.trim().to_string())
        .unwrap_or_else(|_| "HEAD".to_string())
}

/// 作業ツリーの状態を git から読む。
pub fn tree(root: &Path) -> Result<Tree, String> {
    let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let diff = git(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--name-only",
            &base(root),
        ],
    )?;
    let others = git(root, &["ls-files", "--others", "--exclude-standard"])?;
    Ok(Tree {
        branch: branch.trim().to_string(),
        changed: diff
            .lines()
            .chain(others.lines())
            .map(str::to_string)
            .collect(),
    })
}

/// [`notices`] に、`SPEC.ja.md` と `SPEC.en.md` がテストと合っていないことの知らせを足す。
/// SPEC はテストとストーリーと語の表から作るので、それらを変えたときだけ確かめる。
pub fn all(root: &Path, tree: &Tree) -> Vec<Notice> {
    let spec_inputs = tree
        .changed
        .iter()
        .any(|path| is_test(path) || path == "stories.toml" || path == "glossary.toml");
    let stale: Vec<String> = match spec_inputs {
        true => spec::Lang::ALL
            .iter()
            .filter_map(|&lang| spec::check(root, lang).err())
            .collect(),
        false => vec![],
    };
    let spec = match stale.is_empty() {
        true => vec![],
        false => vec![Notice::SpecStale(stale.join("\n"))],
    };
    notices(tree).into_iter().chain(spec).collect()
}

/// [`BASE`] から足したテストの文。
pub fn texts(root: &Path) -> Result<Vec<String>, String> {
    git(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "-U0",
            &base(root),
            "--",
            "crates/leani/tests",
            "tests",
        ],
    )
    .map(|diff| added_texts(&diff))
}
