//! Tab 補完の候補を集める (読み取り)。
//!
//! 何を補完するかは `leani_core::pure::completion_target` が決める。ここでは、ファイルと
//! PATH を読まないと分からない候補 (パスと shell のコマンド) を集め、残りの候補を
//! 引数の関数 (定数名はエンジンに問い合わせる、環境の名前は設定から) で受け取って、
//! 1 つの一覧にする。

use std::path::Path;

use leani_core::pure::{completion_target, pick};
use leani_core::types::{Candidate, Target};

use crate::places::Ctx;

/// カーソルの前の文字列 `text` に対する候補。
///
/// `names` は prefix で始まる定数名を返す (エンジンに問い合わせる)。定数名を補完する
/// ときだけ呼ぶので、略記やコマンドの途中では問い合わせない。`envs` は設定ファイルに
/// 書いた環境の名前、`meta` は `:` コマンドの名前の一覧。
pub fn candidates(
    ctx: &Ctx,
    text: &str,
    names: &mut dyn FnMut(&str) -> Vec<String>,
    envs: &dyn Fn() -> Vec<String>,
    meta: &[String],
) -> Vec<Candidate> {
    // パスとコマンドの候補は名前の残りだけなので、置き換える範囲は空 (カーソルの位置)。
    let (start, texts) = match completion_target(text) {
        Target::Abbrev { start, symbols } => (start, symbols),
        Target::Shell {
            word,
            command: true,
            ..
        } => (text.len(), commands(ctx, &word)),
        Target::Shell { word, .. } => (text.len(), paths(ctx, &word, false)),
        Target::Meta { start, prefix } => (start, pick(meta, &prefix)),
        Target::Path { word, .. } => (text.len(), paths(ctx, &word, true)),
        Target::Env { start, prefix } => (start, pick(&envs(), &prefix)),
        Target::Name { start, prefix } => (start, names(&prefix)),
    };
    texts
        .into_iter()
        .map(|text| Candidate { start, text })
        .collect()
}

/// `word` の続きになるパスの候補。`lean_only` なら `.lean` ファイルとディレクトリだけに
/// する (`:l` と `:save` で読み書きするのはそれだけなので)。
///
/// 候補は `word` を置き換える形ではなく、ファイル名の残りだけを返す
/// (prompt_toolkit の `PathCompleter` と同じ)。`start` は `word` の末尾に
/// なるので、`candidates` では `word` の末尾を渡す。
pub fn paths(ctx: &Ctx, word: &str, lean_only: bool) -> Vec<String> {
    let word = expand_user(ctx, word);
    let (dir, prefix) = split_dir(&word);
    let keep = |full: &Path| !lean_only || full.is_dir() || word_ends_lean(full);
    entries(ctx, dir, prefix)
        .into_iter()
        .filter(|(full, _)| keep(full))
        .map(|(_, rest)| rest)
        .collect()
}

/// `word` で始まる、PATH にある実行できるコマンドの候補。`word` が空なら出さない。
pub fn commands(ctx: &Ctx, word: &str) -> Vec<String> {
    if word.is_empty() {
        return Vec::new();
    }
    let executable = |full: &Path| {
        full.is_dir()
            || std::fs::metadata(full).is_ok_and(|m| {
                use std::os::unix::fs::PermissionsExt;
                m.permissions().mode() & 0o111 != 0
            })
    };
    let mut found: Vec<String> = ctx
        .path
        .split(':')
        .flat_map(|dir| entries_in(Path::new(dir), word))
        .filter(|(full, _)| executable(full))
        .map(|(_, rest)| rest)
        .collect();
    found.sort();
    found
}

/// 先頭の `~` をホームディレクトリにする。
fn expand_user(ctx: &Ctx, word: &str) -> String {
    match word.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{}{rest}", ctx.home),
        _ => word.to_string(),
    }
}

/// `bin/leanf` を (`bin/`, `leanf`) に分ける。`/` が無ければディレクトリは空。
fn split_dir(word: &str) -> (&str, &str) {
    match word.rfind('/') {
        Some(i) => (&word[..=i], &word[i + 1..]),
        None => ("", word),
    }
}

fn word_ends_lean(full: &Path) -> bool {
    full.to_string_lossy().ends_with(".lean")
}

/// `dir` (空ならカレントディレクトリ) にある、`prefix` で始まるファイルの (パス, 名前の
/// 残り) を名前の順に返す。
fn entries(ctx: &Ctx, dir: &str, prefix: &str) -> Vec<(std::path::PathBuf, String)> {
    let base = if dir.is_empty() {
        Path::new(&ctx.cwd).to_path_buf()
    } else {
        Path::new(&ctx.cwd).join(dir)
    };
    let mut found = entries_in(&base, prefix);
    found.sort_by(|a, b| a.1.cmp(&b.1));
    found
}

fn entries_in(dir: &Path, prefix: &str) -> Vec<(std::path::PathBuf, String)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read.filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_prefix(prefix)
                .map(|rest| (e.path(), rest.to_string()))
        })
        .collect()
}
