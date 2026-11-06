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
//! | サブコマンド             | 使う所                                                                                    |
//! |--------------------------|-------------------------------------------------------------------------------------------|
//! | `post-edit`              | PostToolUse の hook (Edit と Write)。変えたファイルに関係する知らせを返す                 |
//! | `stop`                   | Stop の hook。知らせがあれば、AI が終わる前に 1 回だけ止めて、知らせを読ませる            |
//! | `status`                 | `just status` とスキル。今の知らせを、人が読む形で出す                                    |
//! | `texts`                  | スキル leani-test-text。`main-rs` から足したテストの文を出し、`texts.txt` にも書く        |
//! | `base`                   | `just finish`。枝を切る元の枝 ([`BASE`]) の名前を出す                                     |
//! | `can-start <名前>`       | `just start`。枝を切ってよいかを確かめる ([`can_start`])                                  |
//! | `can-finish [--head X]`  | `just finish`。merge してよいかを確かめる ([`can_finish`] と [`bad_subjects`])            |
//! | `after-finish <前> <後>` | `just finish`。merge で変わったファイルを見て、インストールし直しを頼む                   |
//! | `pairs [--verify]`       | スキル leani-docs-sync。片方だけを変えた文書を出す ([`pairs`] と [`verify_pairs`])        |
//! | `texts-verify`           | スキル leani-test-text。AI の判断 (`texts.json`) を確かめる ([`verify_texts`])            |
//! | `lint-ja <ファイル…>`    | `just lint-ja`。yomiyasu の lint を動かし、決まりと食い違う指摘を外す ([`kept_findings`]) |
//! | `texts-lint`             | スキル leani-test-text。足した `ja:` の文を yomiyasu の lint にかける                     |
//!
//! 1 つのセッションの中では、同じ知らせを hook ごとに 1 回だけ返す。hook の入力と応答は
//! [`STATE`] の `log.jsonl` に残す。
//!
//! `base` から `after-finish` までは読むだけで、枝を切る、merge する、枝を消すは `justfile` の
//! レシピが行う。確かめに通らなければ、理由を出して 1 で終わる。

use std::path::{Path, PathBuf};
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
                 `just start <枝の名前>` で枝を切ってから作業する。"
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

/// 枝の名前が detached HEAD を表すか。`git branch --show-current` は空の文字列を、
/// `git rev-parse --abbrev-ref HEAD` は `HEAD` を返す。
fn detached(branch: &str) -> bool {
    matches!(branch, "" | "HEAD")
}

/// 今の枝が branch のときに、[`BASE`] から枝を切ってよいか。`git switch -c` は commit していない
/// 変更を新しい枝に持っていくので、変更があっても通す。
pub fn can_start(branch: &str) -> Result<(), String> {
    match branch {
        b if detached(b) => Err(format!(
            "detached HEAD の上にいる。`git switch {BASE}` してから `just start` する。"
        )),
        BASE => Ok(()),
        b if BASES.contains(&b) => Err(format!(
            "{b} の上にいる。`git switch {BASE}` してから `just start` する。"
        )),
        b => Err(format!(
            "既に枝 {b} の上にいる。枝を切らずに、そのまま続ける。"
        )),
    }
}

/// 自分の環境のためのファイル。commit しないので、追跡していなくても merge を止めない。
pub const PERSONAL: &[&str] = &[".envrc"];

/// merge する前の作業ツリーの状態。
#[derive(Debug, PartialEq)]
pub struct Finish {
    /// 今の枝の名前。detached HEAD なら空の文字列か `HEAD`。
    pub branch: String,
    /// 追跡しているファイルのうち、commit していない変更があるもの。
    pub dirty: Vec<String>,
    /// 追跡していないファイル (`.gitignore` で外したものは含まない)。
    pub untracked: Vec<String>,
    /// [`BASE`] が `HEAD` の祖先か。祖先なら fast-forward で merge できる。
    pub base_is_ancestor: bool,
    /// `just check` の前に記録した `HEAD` の commit。無ければ比べない。
    pub recorded: Option<String>,
    /// 今の `HEAD` の commit。
    pub head: String,
}

/// 枝を [`BASE`] に merge してよいか。止める理由を、確かめる順に 1 つだけ返す。
pub fn can_finish(given: &Finish) -> Result<(), String> {
    let untracked: Vec<&str> = given
        .untracked
        .iter()
        .map(String::as_str)
        .filter(|path| !PERSONAL.contains(path))
        .collect();
    match given {
        f if detached(&f.branch) => {
            Err("detached HEAD の上にいる。merge する枝に移る。".to_string())
        }
        f if BASES.contains(&f.branch.as_str()) => {
            Err(format!("{} の上にいる。merge する枝に移る。", f.branch))
        }
        f if !f.dirty.is_empty() => Err(format!(
            "commit していない変更がある。commit してから `just finish` する: {}",
            f.dirty.join(", ")
        )),
        _ if !untracked.is_empty() => Err(format!(
            "追跡していないファイルがある。commit するか消してから `just finish` する: {}",
            untracked.join(", ")
        )),
        f if !f.base_is_ancestor => Err(format!(
            "{BASE} が HEAD の祖先でないので、fast-forward で merge できない。\
             `git rebase {BASE}` してから `just finish` する。"
        )),
        Finish {
            recorded: Some(recorded),
            head,
            ..
        } if recorded != head => Err(format!(
            "`just check` の途中で HEAD が {recorded} から {head} に変わった。もう一度 `just finish` する。"
        )),
        _ => Ok(()),
    }
}

/// commit の件名に使う Conventional Commits の型 (CLAUDE.md の「Git運用」)。
const TYPES: &[&str] = &["feat", "fix", "docs", "chore", "refactor", "test"];

/// `git log --format=%s` の出力から、型と `: ` で始まらない件名と、日本語の文字を含まない件名を返す。
pub fn bad_subjects(log: &str) -> Vec<String> {
    log.lines()
        .filter(|subject| !subject.is_empty())
        .filter(|subject| {
            let typed = TYPES.iter().any(|t| subject.starts_with(&format!("{t}: ")));
            !(typed && subject.chars().any(japanese))
        })
        .map(str::to_string)
        .collect()
}

/// ひらがな、カタカナ、漢字か。
fn japanese(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}')
}

/// hook が動かす claude-hooks のコードと、それが依存する crate。
const HOOK_SOURCES: &[&str] = &["crates/claude-hooks/", "crates/spec/", "crates/docs-pair/"];

/// merge で changed のファイルが変わったときに、ユーザーに頼む文。hook はインストールした
/// claude-hooks で動くので、hook のコードが変わったらインストールし直してもらう。
/// AI はインストールしない (スキル leani-dev の「hook からの知らせ」)。
pub fn reinstall(changed: &[String]) -> Option<String> {
    let touched = changed.iter().any(|path| {
        HOOK_SOURCES.iter().any(|dir| path.starts_with(dir))
            || matches!(path.as_str(), "Cargo.toml" | "Cargo.lock")
    });
    match touched {
        true => Some(
            "hook のコードが変わった。ユーザーに `cargo install --path crates/claude-hooks` を\
             実行してもらう。"
                .to_string(),
        ),
        false => None,
    }
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

/// hook は sandbox の外で動くので、リポジトリの `.git/config` に書いたコマンドを git に実行させない。
/// 主に防ぐのは [`guard`] で、ここで fsmonitor を止め、`git diff` に `--no-ext-diff` と
/// `--no-textconv` を付けるのは、[`guard`] の既知のキーに漏れがあったときのためである。
/// `git diff` には `--ignore-submodules=all` も付ける。[`guard`] は submodule の config を
/// 確かめないので、git を submodule の中で動かさないためである。
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

/// コマンドを動かさないことが分かっている、リポジトリの config のキー。
fn known(key: &str) -> bool {
    matches!(
        key,
        "core.repositoryformatversion"
            | "core.filemode"
            | "core.bare"
            | "core.logallrefupdates"
            | "core.ignorecase"
            | "core.precomposeunicode"
    ) || key.starts_with("user.")
        || (key.starts_with("remote.") && (key.ends_with(".url") || key.ends_with(".fetch")))
        || (key.starts_with("branch.") && (key.ends_with(".remote") || key.ends_with(".merge")))
}

/// `git config --list --show-scope --name-only -z` の出力から、リポジトリの config (local と
/// worktree) にある、コマンドを動かさないと分かっていないキーを返す。
///
/// 出力は、scope とキーを NUL で区切って交互に並べたものである。subsection の名前には `=` や
/// 改行を書けるので、値と一緒に出す形 (`scope\tkey=value` の行) から `=` で切り出すと、
/// `remote.a.url=b.uploadpack` を `remote.a.url` と読み違える。
///
/// hook は sandbox の外で動く。`.git/config` に filter や fsmonitor のコマンドを書かれると、
/// `git diff` がそのコマンドを実行する。止めるキーを並べると漏れが残るので、動かしてよいキーを並べ、
/// それ以外のキーがあれば git を動かさない (DECISIONS.ja.md)。ユーザーの global と system の
/// config は、ユーザーが自分で書いたものなので確かめない。
pub fn unknown_keys(list: &str) -> Vec<String> {
    list.split('\0')
        .collect::<Vec<_>>()
        .chunks_exact(2)
        .filter(|pair| matches!(pair[0], "local" | "worktree"))
        .map(|pair| pair[1])
        .filter(|key| !known(key))
        .map(str::to_string)
        .collect()
}

/// リポジトリの config に [`unknown_keys`] があれば、エラーを返す。git を動かす前に呼ぶ。
fn guard(root: &Path) -> Result<(), String> {
    let list = git(
        root,
        &["config", "--list", "--show-scope", "--name-only", "-z"],
    )?;
    match unknown_keys(&list).as_slice() {
        [] => Ok(()),
        keys => Err(format!(
            "リポジトリの git の config に、コマンドを動かさないと分かっていないキーがあるので、git を動かさない: {}",
            keys.join(", ")
        )),
    }
}

/// [`BASE`] と今の commit が分かれた commit。[`BASE`] が無ければ `HEAD`。[`guard`] のあとに呼ぶ。
fn base(root: &Path) -> String {
    git(root, &["merge-base", "HEAD", BASE])
        .map(|id| id.trim().to_string())
        .unwrap_or_else(|_| "HEAD".to_string())
}

/// 作業ツリーの状態を git から読む。
pub fn tree(root: &Path) -> Result<Tree, String> {
    guard(root)?;
    let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let diff = git(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=all",
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
    guard(root)?;
    git(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=all",
            "-U0",
            &base(root),
            "--",
            "crates/leani/tests",
            "tests",
        ],
    )
    .map(|diff| added_texts(&diff))
}

/// 今の枝の名前。detached HEAD なら空の文字列。[`guard`] のあとに呼ぶ。
fn branch(root: &Path) -> Result<String, String> {
    git(root, &["branch", "--show-current"]).map(|name| name.trim().to_string())
}

/// `just start` の前の確かめ。name を枝の名前に使えるかと、[`can_start`] を確かめる。
pub fn start(root: &Path, name: &str) -> Result<(), String> {
    guard(root)?;
    git(root, &["check-ref-format", "--branch", name])
        .map_err(|_| format!("枝の名前に使えない: {name}"))?;
    can_start(&branch(root)?)
}

/// `just finish` の merge の前の確かめ。[`can_finish`] と、[`BASE`] から足した commit の
/// 件名 ([`bad_subjects`]) を確かめる。recorded は `just check` の前に記録した `HEAD`。
pub fn finish(root: &Path, recorded: Option<&str>) -> Result<(), String> {
    guard(root)?;
    let lines = |out: String| out.lines().map(str::to_string).collect::<Vec<_>>();
    let given = Finish {
        branch: branch(root)?,
        dirty: git(
            root,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--ignore-submodules=all",
                "--name-only",
                "HEAD",
            ],
        )
        .map(lines)?,
        untracked: git(root, &["ls-files", "--others", "--exclude-standard"]).map(lines)?,
        // 祖先でなければ git は 1 で終わるので、エラーを「祖先でない」と読む。
        base_is_ancestor: git(root, &["merge-base", "--is-ancestor", BASE, "HEAD"]).is_ok(),
        recorded: recorded.map(str::to_string),
        head: git(root, &["rev-parse", "HEAD"])?.trim().to_string(),
    };
    can_finish(&given)?;
    let range = format!("{BASE}..HEAD");
    let log = git(root, &["log", "--no-show-signature", "--format=%s", &range])?;
    match bad_subjects(&log).as_slice() {
        [] => Ok(()),
        bad => Err(format!(
            "commit の件名を `feat: ` のような型で始め、日本語で書く。\
             `git commit --amend` か `git rebase` で直してから `just finish` する:\n{}",
            bad.join("\n")
        )),
    }
}

/// `just finish` の merge のあとの案内。old は merge の前の [`BASE`]、new は merge した commit。
pub fn after_finish(root: &Path, old: &str, new: &str) -> Result<Option<String>, String> {
    guard(root)?;
    git(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=all",
            "--name-only",
            old,
            new,
        ],
    )
    .map(|out| reinstall(&out.lines().map(str::to_string).collect::<Vec<_>>()))
}

/// 片方だけを変えた文書を見つけた所。
#[derive(Debug, PartialEq)]
pub enum Origin {
    /// [`BASE`] から作業ツリーまでの変更。
    Tree,
    /// 追跡していない新しいファイル。差分は無いので、ファイルを読む。
    NewFile,
    /// 片方だけを変えた commit。中身は docs-pair のエラーの文。
    Commit(String),
}

/// 片方だけを変えた文書と、もう片方の文書に写す差分。
#[derive(Debug, PartialEq)]
pub struct Pair {
    pub changed: String,
    pub other: String,
    pub origin: Origin,
    /// changed の差分。[`Origin::NewFile`] では空。
    pub diff: String,
}

/// 追跡していないファイル。[`guard`] のあとに呼ぶ。
fn untracked(root: &Path) -> Result<Vec<String>, String> {
    git(root, &["ls-files", "--others", "--exclude-standard"])
        .map(|out| out.lines().map(str::to_string).collect())
}

/// `git diff` に付ける、リポジトリの config のコマンドを動かさないための引数 ([`git`])。
const SAFE_DIFF: [&str; 3] = ["--no-ext-diff", "--no-textconv", "--ignore-submodules=all"];

/// 片方だけを変えた文書。作業ツリーの [`Notice::OneSided`] と、[`BASE`] から足した commit のうち
/// docs-pair がエラーにするものを合わせる。
pub fn pairs(root: &Path) -> Result<Vec<Pair>, String> {
    let tree = tree(root)?;
    let base = base(root);
    let new = untracked(root)?;
    let from_tree = notices(&tree)
        .into_iter()
        .filter_map(|n| match n {
            Notice::OneSided { changed, other } => Some((changed, other)),
            _ => None,
        })
        .map(|(changed, other)| match new.contains(&changed) {
            true => Ok(Pair {
                changed,
                other,
                origin: Origin::NewFile,
                diff: String::new(),
            }),
            false => {
                let args: Vec<&str> = std::iter::once("diff")
                    .chain(SAFE_DIFF)
                    .chain([base.as_str(), "--", changed.as_str()])
                    .collect();
                git(root, &args).map(|diff| Pair {
                    changed,
                    other,
                    origin: Origin::Tree,
                    diff,
                })
            }
        });
    let range = format!("{BASE}..HEAD");
    let from_commits = docs_pair::sided_commits(root, &[&range])?
        .into_iter()
        .map(|s| {
            let args: Vec<&str> = ["show", "--format="]
                .into_iter()
                .chain(SAFE_DIFF)
                .chain([s.id.as_str(), "--", s.changed.as_str()])
                .collect();
            git(root, &args).map(|diff| Pair {
                changed: s.changed,
                other: s.other,
                origin: Origin::Commit(s.complaint),
                diff,
            })
        });
    from_tree.chain(from_commits).collect()
}

/// 訳した文書の glossary の語のずれ。ja と en は文書の名前、term は glossary の日本語の語。
#[derive(Debug, PartialEq)]
pub enum Gap {
    /// 英語の文書のどこにも、語の英語が無い。
    Missing {
        ja: String,
        en: String,
        term: String,
    },
    /// 英語の文書にはあるが、英語の差分に無い。語だけを直した場合や、代名詞にした場合があるので、
    /// AI が判断する。
    Candidate {
        ja: String,
        en: String,
        term: String,
    },
}

/// `git diff` の出力のうち、足した行。
pub fn added_lines(diff: &str) -> String {
    diff.lines()
        .filter(|line| !line.starts_with("+++"))
        .filter_map(|line| line.strip_prefix('+'))
        .map(|line| format!("{line}\n"))
        .collect()
}

/// 日本語の文書に足した行 (ja_added) にある glossary の語ごとに、英語の文書の全体 (en_text) と
/// 足した行 (en_added) で英語の語を探す。
pub fn term_gaps(
    glossary: &spec::Glossary,
    (ja, en): (&str, &str),
    ja_added: &str,
    en_text: &str,
    en_added: &str,
) -> Vec<Gap> {
    glossary
        .term
        .iter()
        .filter(|t| t.used_in(ja_added))
        .filter_map(|t| {
            let (ja, en, term) = (ja.to_string(), en.to_string(), t.ja.clone());
            match (t.kept_in(en_text), t.kept_in(en_added)) {
                (false, _) => Some(Gap::Missing { ja, en, term }),
                (true, false) => Some(Gap::Candidate { ja, en, term }),
                (true, true) => None,
            }
        })
        .collect()
}

/// [`BASE`] から作業ツリーまでに変えた日本語の文書ごとに、[`term_gaps`] を調べる。
/// 追跡していない文書は、全体を足した行として扱う。空白だけを変えた行は、足した行に数えない。
pub fn verify_pairs(root: &Path) -> Result<Vec<Gap>, String> {
    let tree = tree(root)?;
    let base = base(root);
    let new = untracked(root)?;
    let glossary = spec::glossary(root)?;
    let read = |path: &str| std::fs::read_to_string(root.join(path)).unwrap_or_default();
    let added = |path: &str| -> Result<String, String> {
        match new.iter().any(|n| n == path) {
            true => Ok(read(path)),
            false => {
                let args: Vec<&str> = std::iter::once("diff")
                    .chain(SAFE_DIFF)
                    // 表の整形のような、空白だけの変更は数えない。
                    .chain(["-U0", "-w", base.as_str(), "--", path])
                    .collect();
                git(root, &args).map(|diff| added_lines(&diff))
            }
        }
    };
    tree.changed
        .iter()
        .filter(|path| path.ends_with(".ja.md") && counts(path) && !docs_pair::exempt(path))
        .filter_map(|ja| docs_pair::partner(ja).map(|en| (ja, en)))
        .map(|(ja, en)| {
            Ok(term_gaps(
                &glossary,
                (ja, &en),
                &added(ja)?,
                &read(&en),
                &added(&en)?,
            ))
        })
        .collect::<Result<Vec<_>, String>>()
        .map(|gaps| gaps.into_iter().flatten().collect())
}

/// [`texts`] の 1 行 (`ファイル:行: 文`) を、場所と文に分ける。
fn place_and_text(line: &str) -> (&str, &str) {
    line.split_once(": ").unwrap_or((line, ""))
}

/// AI の答え (`texts.json` の要素) のキー。
const ANSWER_KEYS: &[&str] = &["place", "text", "verdict", "reason", "fixed"];

/// AI の答えの 1 つの要素。ng は、ng のときの理由と直した文。
struct Answer {
    place: String,
    text: String,
    ng: Option<(String, String)>,
}

/// 答えの 1 つの要素を確かめて返す。
fn answer_item(item: &Value) -> Result<Answer, String> {
    let field = |key: &str| item.get(key).and_then(Value::as_str).map(str::to_string);
    let object = item
        .as_object()
        .ok_or_else(|| format!("要素がオブジェクトでない: {item}"))?;
    let unknown: Vec<&String> = object
        .keys()
        .filter(|k| !ANSWER_KEYS.contains(&k.as_str()))
        .collect();
    let (place, text) = match (field("place"), field("text"), unknown.is_empty()) {
        (Some(place), Some(text), true) => (place, text),
        _ => {
            return Err(format!(
                "place と text が文字列でないか、知らないキーがある: {item}"
            ));
        }
    };
    let given = |key: &str| field(key).filter(|v| !v.trim().is_empty());
    match (
        field("verdict").as_deref(),
        given("reason"),
        given("fixed"),
        object.contains_key("reason") || object.contains_key("fixed"),
    ) {
        (Some("ok"), _, _, false) => Ok(Answer {
            place,
            text,
            ng: None,
        }),
        (Some("ng"), Some(reason), Some(fixed), _) => Ok(Answer {
            place,
            text,
            ng: Some((reason, fixed)),
        }),
        (Some("ok"), _, _, true) => Err(format!("{place}: ok に reason か fixed がある")),
        (Some("ng"), _, _, _) => Err(format!("{place}: ng に reason か fixed が無い")),
        _ => Err(format!("{place}: verdict が ok でも ng でもない")),
    }
}

/// AI の答え (`texts.json` の中身) を、今の [`texts`] の行 (current) と比べて確かめ、ng の文を
/// 1 つずつ人が読む形で返す。答えの場所と文の組は、今の行の組と 1 つずつ対応しなければならない。
/// 文を直したあとの古い答えと、あとから足した文の抜けは、ここでエラーになる。
pub fn verify_texts(current: &[String], answer: &str) -> Result<Vec<String>, String> {
    let items: Vec<Value> = match serde_json::from_str::<Value>(answer) {
        Ok(Value::Array(items)) => items,
        Ok(_) => return Err("texts.json が配列でない".to_string()),
        Err(e) => return Err(format!("texts.json: {e}")),
    };
    let (answers, shape): (Vec<_>, Vec<_>) = items.iter().map(answer_item).partition(Result::is_ok);
    let answers: Vec<_> = answers.into_iter().filter_map(Result::ok).collect();
    let shape = shape.into_iter().filter_map(Result::err);
    let wanted: Vec<(&str, &str)> = current.iter().map(|l| place_and_text(l)).collect();
    let count = |place: &str, text: &str| {
        answers
            .iter()
            .filter(|a| a.place == place && a.text == text)
            .count()
    };
    let missing = wanted
        .iter()
        .filter(|(p, t)| count(p, t) == 0)
        .map(|(p, t)| format!("{p}: 答えに無い: {t}"));
    let twice = wanted
        .iter()
        .filter(|(p, t)| count(p, t) > 1)
        .map(|(p, t)| format!("{p}: 答えに 2 回以上ある: {t}"));
    let extra = answers
        .iter()
        .filter(|a| !wanted.contains(&(a.place.as_str(), a.text.as_str())))
        .map(
            |Answer {
                 place: p, text: t, ..
             }| format!("{p}: 今の文に無い (古い答えか、場所か文の写し間違い): {t}"),
        );
    let errors: Vec<String> = shape.chain(missing).chain(twice).chain(extra).collect();
    match errors.is_empty() {
        true => Ok(answers
            .iter()
            .filter_map(|Answer { place, text, ng }| {
                ng.as_ref().map(|(reason, fixed)| {
                    let rename = match text.starts_with("en:") {
                        true => {
                            "\n  関数名か mod の名前も変える (DEVELOPMENT.ja.md の「作業の流れ」)"
                        }
                        false => "",
                    };
                    format!("{place}: {text}\n  理由: {reason}\n  直した文: {fixed}{rename}")
                })
            })
            .collect()),
        false => Err(errors.join("\n")),
    }
}

/// installed_plugins.json の中の、yomiyasu の plugin のキー。
const YOMIYASU: &str = "yomiyasu@yomiyasu";

/// installed_plugins.json の中身 (raw) から、yomiyasu の plugin の場所 (installPath) と版を選ぶ。
/// リポジトリの直下 (root) の project の要素を、user の要素より先に選ぶ。
pub fn pick_lint(raw: &str, root: &Path) -> Option<(PathBuf, String)> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let entries = value.get("plugins")?.get(YOMIYASU)?.as_array()?;
    let text = |e: &Value, key: &str| e.get(key).and_then(Value::as_str).map(str::to_string);
    let scoped = |scope: &str| {
        entries.iter().find(|e| {
            text(e, "scope").as_deref() == Some(scope)
                && match scope {
                    "project" => text(e, "projectPath").map(PathBuf::from).as_deref() == Some(root),
                    _ => true,
                }
        })
    };
    scoped("project")
        .or_else(|| scoped("user"))
        .and_then(|e| Some((PathBuf::from(text(e, "installPath")?), text(e, "version")?)))
}

/// yomiyasu の lint の指摘のうち、leani-dev で使うもの。
#[derive(Debug, PartialEq)]
pub struct Finding {
    pub line: u64,
    pub rule: String,
    pub message: String,
}

/// DEVELOPMENT.ja.md の決まりと食い違うので、外す規則。
const IGNORED_RULES: &[&str] = &[
    // 「文書の書き方」の「英数字の前後には半角の空白を入れる」と食い違う (1.0.3 だけにある)。
    "unnatural_halfwidth_space",
    // 箇条書きと表のほうが読みやすい所は、そのまま残す (ユーザーの指示)。
    "excess_list",
];

/// yomiyasu の lint の `--json` の出力 (raw) から、[`IGNORED_RULES`] を除いた指摘を返す。
/// 1.0.3 と 2.4.0 で形が違うので、どちらにもある `findings` の `rule`、`line`、`message` だけを読む。
pub fn kept_findings(raw: &str) -> Result<Vec<Finding>, String> {
    let value: Value = serde_json::from_str(raw).map_err(|e| format!("lint の出力: {e}"))?;
    let findings = value
        .get("findings")
        .and_then(Value::as_array)
        .ok_or("lint の出力に findings が無い")?;
    findings
        .iter()
        .map(|f| {
            let text = |key: &str| f.get(key).and_then(Value::as_str).map(str::to_string);
            match (
                text("rule"),
                f.get("line").and_then(Value::as_u64),
                text("message"),
            ) {
                (Some(rule), Some(line), Some(message)) => Ok(Finding {
                    line,
                    rule,
                    message,
                }),
                _ => Err(format!("lint の指摘の形が違う: {f}")),
            }
        })
        .filter(|f| match f {
            Ok(f) => !IGNORED_RULES.contains(&f.rule.as_str()),
            Err(_) => true,
        })
        .collect()
}

/// 使う yomiyasu の lint のスクリプトと版。given は環境変数 `YOMIYASU_LINT` の値で、あればそれを使う。
/// 無ければ、Claude Code の設定のディレクトリ (config) の `plugins/installed_plugins.json` から選ぶ。
pub fn find_lint(
    root: &Path,
    given: Option<PathBuf>,
    config: &Path,
) -> Result<(PathBuf, String), String> {
    let installed = config.join("plugins/installed_plugins.json");
    match given {
        Some(path) => Ok((path, "不明".to_string())),
        None => std::fs::read_to_string(&installed)
            .ok()
            .and_then(|raw| pick_lint(&raw, root))
            .map(|(dir, version)| (dir.join("scripts/yomiyasu_lint.py"), version))
            .ok_or(format!(
                "yomiyasu の lint が見つからない。{} に {YOMIYASU} が無い。\
                 plugin を入れるか、環境変数 YOMIYASU_LINT にスクリプトのパスを書く",
                installed.display()
            )),
    }
}

/// yomiyasu の lint (script) を 1 つのファイルに動かし、[`kept_findings`] を返す。
pub fn lint_file(root: &Path, script: &Path, file: &Path) -> Result<Vec<Finding>, String> {
    let out = Command::new("python3")
        .arg(script)
        .arg(file)
        .arg("--json")
        .current_dir(root)
        .output()
        .map_err(|e| format!("python3: {e}"))?;
    // 指摘があるときに 1 で終わる版があるので、0 と 1 は出力を読む。
    match out.status.code() {
        Some(0 | 1) => kept_findings(&String::from_utf8_lossy(&out.stdout)),
        _ => Err(format!(
            "{}: {}",
            file.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}
