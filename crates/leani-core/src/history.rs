//! 履歴ファイルの形式 (純粋)。
//!
//! Python 版は prompt_toolkit の `FileHistory` の形式をそのまま使っていた。Rust 版も
//! 同じ形式で読み書きする。既存の履歴ファイルをそのまま読めて、追記だけで済ませる
//! ためである。
//!
//! ```text
//!
//! # 2026-10-26 12:34:56.789012
//! +def f : Nat -> Nat
//! +  | 0 => 1
//! ```
//!
//! 1 件は空行、`# 時刻` の行、`+` で始まる行の並びである。`+` で始まらない行は
//! 件の区切りとして扱い、中身は読まない。readline や libedit の履歴のように
//! 形式の違うファイルがあっても、読める行だけを読む。

/// 1 件を追記する形にする。`stamp` は記録した時刻の文字列。
pub fn history_entry(stamp: &str, src: &str) -> String {
    let lines: String = src.split('\n').map(|l| format!("+{l}\n")).collect();
    format!("\n# {stamp}\n{lines}")
}

/// 履歴ファイルの中身から、記録した順に件を取り出す。
///
/// `+` で始まる行の並びが 1 件である。`+` で始まらない行はすべて区切りとして
/// 扱うので、読めない形式の行は無視される。
pub fn history_strings(text: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut lines: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        match line.strip_prefix('+') {
            Some(body) => lines.push(body),
            None => {
                if !lines.is_empty() {
                    entries.push(lines.join("\n"));
                }
                lines.clear();
            }
        }
    }
    if !lines.is_empty() {
        entries.push(lines.join("\n"));
    }
    entries
}
