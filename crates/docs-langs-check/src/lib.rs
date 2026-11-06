//! 英語の文書 `X.md` と日本語の文書 `X.ja.md` が対になっていることを確かめる。
//!
//! どちらの文書も原本として扱い、同じ commit で両方を変える (DECISIONS.ja.md の「文書」)。
//! [`check`] は次の 2 つをエラーにする。
//!
//! - もう片方の言語の文書が無い文書
//! - 対の両方があったのに、片方だけを変えた commit。commit メッセージに `Docs-Langs-Skip:`
//!   で始まる行があれば、エラーにしない
//!
//! 確かめるのは commit だけで、commit していない変更は確かめない。片方の言語の文書を
//! 直している途中でも `just check` を実行できるようにするためである。
//! 訳の中身が合っているかは確かめない。

use std::path::{Path, PathBuf};
use std::process::Command;

/// commit メッセージにこの語で始まる行があれば、片方だけを変えてもエラーにしない。
pub const SKIP: &str = "Docs-Langs-Skip:";

/// リポジトリの直下。cwd によらずに決まるように、この crate の場所から求める。
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// 対を求めない文書。`CLAUDE.md` は Claude Code が決まった名前で読み、`SPEC.ja.md` と
/// `SPEC.en.md` は `crates/spec` が両方を生成する。
pub fn exempt(path: &str) -> bool {
    matches!(path, "CLAUDE.md" | "SPEC.ja.md" | "SPEC.en.md") || path.starts_with(".claude/")
}

/// 対になるもう片方の文書の名前。`X.ja.md` には `X.md` を、`X.md` には `X.ja.md` を返す。
pub fn partner(path: &str) -> Option<String> {
    match (path.strip_suffix(".ja.md"), path.strip_suffix(".md")) {
        (Some(stem), _) => Some(format!("{stem}.md")),
        (None, Some(stem)) => Some(format!("{stem}.ja.md")),
        (None, None) => None,
    }
}

/// もう片方の言語の文書が無い文書ごとのエラーの文。
pub fn unpaired(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| !exempt(path))
        .filter_map(|path| partner(path).map(|other| (path, other)))
        .filter(|(_, other)| !paths.contains(other))
        .map(|(path, other)| format!("{path} に対になる {other} が無い"))
        .collect()
}

/// `git log` の 1 つの commit。
#[derive(Debug, PartialEq)]
pub struct Commit {
    pub id: String,
    /// 最初の親。最初の commit には無い。
    pub parent: Option<String>,
    pub message: String,
    /// この commit が変えた文書。
    pub files: Vec<String>,
}

/// `git log` の書式。commit を `\x01` で始め、ID と親とメッセージを `\x00` で区切り、
/// メッセージを `\x02` で閉じる。そのあとに `--name-only` のファイル名が続く。
const FORMAT: &str = "--format=%x01%H%x00%P%x00%B%x02";

/// [`FORMAT`] の `git log` の出力を commit に分ける。
pub fn commits(log: &str) -> Vec<Commit> {
    log.split('\x01')
        .filter_map(|record| {
            let (head, names) = record.split_once('\x02')?;
            let mut fields = head.splitn(3, '\x00');
            let id = fields.next()?.to_string();
            let parent = fields.next()?.split_whitespace().next().map(String::from);
            let message = fields.next()?.to_string();
            let files = names
                .lines()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(String::from)
                .collect();
            Some(Commit {
                id,
                parent,
                message,
                files,
            })
        })
        .collect()
}

/// commit が片方だけを変えた対 (変えた文書、変えなかった文書)。親の commit に対の両方が
/// あったかは、呼ぶ側が確かめる。メッセージに [`SKIP`] の行があるときと、親が無いときは
/// 空を返す。
pub fn one_sided(commit: &Commit) -> Vec<(String, String)> {
    match (
        &commit.parent,
        commit.message.lines().any(|line| line.starts_with(SKIP)),
    ) {
        (Some(_), false) => commit
            .files
            .iter()
            .filter(|path| !exempt(path))
            .filter_map(|path| partner(path).map(|other| (path.clone(), other)))
            .filter(|(_, other)| !commit.files.contains(other))
            .collect(),
        _ => Vec::new(),
    }
}

/// 片方だけを変えた commit のエラーの文。
fn complaint(commit: &Commit, path: &str, other: &str) -> String {
    let short: String = commit.id.chars().take(7).collect();
    let subject = commit.message.lines().next().unwrap_or("");
    format!(
        "{short} {subject}: {path} だけを変えて、{other} を変えていない。\
         同じ commit で {other} も変えるか、変えない理由を「{SKIP} 理由」の行で commit メッセージに書く"
    )
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotepath=false"])
        .args(args)
        .output()
        .map_err(|e| format!("git を実行できない: {e}"))?;
    match out.status.success() {
        true => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        false => Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        )),
    }
}

/// 親の commit に `path` と `other` の両方があったか。
fn both_in(root: &Path, parent: &str, path: &str, other: &str) -> Result<bool, String> {
    git(root, &["ls-tree", "--name-only", parent, "--", path, other])
        .map(|out| out.lines().count() == 2)
}

/// 対の両方があったのに、片方だけを変えた commit。
#[derive(Debug, PartialEq)]
pub struct Sided {
    /// commit の ID。
    pub id: String,
    /// 変えた文書。
    pub changed: String,
    /// 変えなかった文書。
    pub other: String,
    /// エラーの文。
    pub complaint: String,
}

/// range の commit のうち、対の両方があったのに片方だけを変えたもの。range は `git log` に
/// 渡す範囲 (`main-rs..HEAD` など) で、空ならすべての commit を見る。
pub fn sided_commits(root: &Path, range: &[&str]) -> Result<Vec<Sided>, String> {
    let args: Vec<&str> = ["log", "--no-merges", "--no-renames", FORMAT, "--name-only"]
        .into_iter()
        .chain(range.iter().copied())
        .chain(["--", "*.md"])
        .collect();
    let log = git(root, &args)?;
    commits(&log)
        .iter()
        .flat_map(|commit| {
            one_sided(commit)
                .into_iter()
                .map(move |(path, other)| (commit, path, other))
        })
        .map(|(commit, path, other)| {
            let parent = commit.parent.as_deref().unwrap_or_default();
            both_in(root, parent, &path, &other).map(|both| {
                both.then(|| Sided {
                    id: commit.id.clone(),
                    complaint: complaint(commit, &path, &other),
                    changed: path,
                    other,
                })
            })
        })
        .filter_map(Result::transpose)
        .collect()
}

/// `root` のリポジトリの文書が対になっているかを確かめる。エラーの文は 1 行に 1 つ。
pub fn check(root: &Path) -> Result<(), String> {
    let tracked: Vec<String> = git(root, &["ls-files", "--", "*.md"])?
        .lines()
        .map(String::from)
        .collect();
    let sided = sided_commits(root, &[])?;
    let bad: Vec<String> = unpaired(&tracked)
        .into_iter()
        .chain(sided.into_iter().map(|s| s.complaint))
        .collect();
    match bad.is_empty() {
        true => Ok(()),
        false => Err(bad.join("\n")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn commit_of(message: &str, files: &[&str]) -> Commit {
        Commit {
            id: "0123456789".to_string(),
            parent: Some("abc".to_string()),
            message: message.to_string(),
            files: strings(files),
        }
    }

    #[test]
    fn pairs_x_md_with_x_ja_md_in_both_directions() {
        assert_eq!(
            partner("guide/proof.md").as_deref(),
            Some("guide/proof.ja.md")
        );
        assert_eq!(
            partner("guide/proof.ja.md").as_deref(),
            Some("guide/proof.md")
        );
        assert_eq!(partner("justfile"), None);
    }

    #[test]
    fn reports_documents_without_a_partner_except_the_exempt_ones() {
        let paths = strings(&[
            "README.md",
            "README.ja.md",
            "DECISIONS.md",
            "guide/proof.ja.md",
            "CLAUDE.md",
            "SPEC.ja.md",
            ".claude/skills/a/SKILL.md",
        ]);
        assert_eq!(
            unpaired(&paths),
            strings(&[
                "DECISIONS.md に対になる DECISIONS.ja.md が無い",
                "guide/proof.ja.md に対になる guide/proof.md が無い",
            ])
        );
    }

    #[test]
    fn splits_the_log_into_commits_with_their_files() {
        let log = "\x01aaa\x00ppp qqq\x00docs: a\n\nbody\n\x02\n\nREADME.md\nREADME.ja.md\n\
                   \x01bbb\x00\x00first\n\x02\n\nREADME.md\n";
        assert_eq!(
            commits(log),
            vec![
                Commit {
                    id: "aaa".to_string(),
                    parent: Some("ppp".to_string()),
                    message: "docs: a\n\nbody\n".to_string(),
                    files: strings(&["README.md", "README.ja.md"]),
                },
                Commit {
                    id: "bbb".to_string(),
                    parent: None,
                    message: "first\n".to_string(),
                    files: strings(&["README.md"]),
                },
            ]
        );
    }

    #[test]
    fn finds_the_pairs_a_commit_changed_on_one_side_only() {
        let commit = commit_of(
            "docs: a",
            &[
                "README.md",
                "README.ja.md",
                "guide/proof.ja.md",
                "CLAUDE.md",
            ],
        );
        assert_eq!(
            one_sided(&commit),
            vec![(
                "guide/proof.ja.md".to_string(),
                "guide/proof.md".to_string()
            )]
        );
    }

    #[test]
    fn finds_nothing_in_a_commit_with_the_skip_line_or_without_a_parent() {
        let skipped = commit_of("docs: a\n\nDocs-Langs-Skip: 誤字だけ\n", &["README.md"]);
        let first = Commit {
            parent: None,
            ..commit_of("docs: a", &["README.md"])
        };
        assert_eq!(one_sided(&skipped), Vec::new());
        assert_eq!(one_sided(&first), Vec::new());
    }

    #[test]
    fn names_the_commit_and_both_documents_in_the_complaint() {
        let text = complaint(
            &commit_of("docs: a\n\nbody", &[]),
            "README.ja.md",
            "README.md",
        );
        assert!(
            text.starts_with("0123456 docs: a: README.ja.md だけを変えて"),
            "{text}"
        );
        assert!(text.contains(SKIP), "{text}");
    }
}
