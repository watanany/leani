//! 判定と整形 (純粋)。
//!
//! 色付け、応答の読み取り、エンジンのバージョン、完結判定、表示の組み立てを扱う。
//! 入力だけで出力が決まるので、テストは入力と出力だけで書ける。
//!
//! repl の応答は `Json` のまま受け取る。キーが無いときや型が違うときは、既定値として
//! 読む (`text`、`int`、`list`)。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;

use crate::abbrev::{abbrev_candidates, abbrev_head, char_width};
use crate::queries::{
    BLOCK_OPEN, CANNOT_EVAL, DECL_NAME, ERR_POS, INCOMPLETE, NESTED_ACTION, NO_REPR, NONCOMPUTABLE,
};
use crate::types::{EngineError, Header, Json, Kind, Proof, Scope, State, Target};

// ------------------------------------------------------------------ 応答の読み取り

pub fn text<'a>(v: &'a Json, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Json::as_str)
}

pub fn int(v: &Json, key: &str) -> Option<i64> {
    v.get(key).and_then(Json::as_i64)
}

pub fn list<'a>(v: &'a Json, key: &str) -> &'a [Json] {
    v.get(key)
        .and_then(Json::as_array)
        .map_or(&[], Vec::as_slice)
}

// ------------------------------------------------------------------ 色

/// 色を付けるかどうか。判定は起動時に 1 回だけ行い (`leani-read`)、ここから下の関数には
/// 引数で渡す。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub color: bool,
}

impl Style {
    pub const PLAIN: Style = Style { color: false };

    pub fn c(self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn red(self, s: &str) -> String {
        self.c("31", s)
    }

    pub fn yellow(self, s: &str) -> String {
        self.c("33", s)
    }

    pub fn green(self, s: &str) -> String {
        self.c("32", s)
    }

    pub fn dim(self, s: &str) -> String {
        self.c("2", s)
    }
}

// ------------------------------------------------------------------ Lean の文字列

/// 文字列を Lean の文字列リテラルにする。
///
/// JSON のエスケープは使わない。JSON は制御文字を `\b` や `\f` と書くが、Lean は
/// これを受け付けない。Lean が読めるのは `\\ \" \n \t \r \uXXXX` などなので、
/// ほかの制御文字は `\uXXXX` にする。
pub fn lean_str(s: &str) -> String {
    let body: String = s.chars().map(lean_char).collect();
    format!("\"{body}\"")
}

fn lean_char(c: char) -> String {
    match c {
        '\\' => "\\\\".to_string(),
        '"' => "\\\"".to_string(),
        '\n' => "\\n".to_string(),
        '\t' => "\\t".to_string(),
        '\r' => "\\r".to_string(),
        c if c.is_control() => format!("\\u{:04x}", u32::from(c)),
        c => c.to_string(),
    }
}

/// 文字列の列を Lean の `#[...]` にする。
pub fn lean_strs<S: AsRef<str>>(xs: &[S]) -> String {
    let items: Vec<String> = xs.iter().map(|x| lean_str(x.as_ref())).collect();
    format!("#[{}]", items.join(", "))
}

// ------------------------------------------------------------------ 補完

/// text の末尾にある名前が始まる位置 (byte)。補完でカーソルの前の名前を切り出すため。
///
/// 末尾から、名前に使える文字が続く範囲を名前にする。区切り文字を並べる方法では、
/// 一覧に無い記号 (`@`、`¬`、`⟨` など) を名前に含めてしまう。Lean の名前は `.` を
/// 含むので、`.` では区切らない。`«` で囲んだ部分は空白や記号を含んでも名前の一部で、
/// 閉じていない `«` からカーソルまでも名前に含める。
pub fn name_start(text: &str) -> usize {
    match open_guillemet(text) {
        Some(i) => name_run(&text[..i]),
        None => name_run(text),
    }
}

/// 閉じていない `«` の位置。
fn open_guillemet(text: &str) -> Option<usize> {
    let open = text.rfind('«')?;
    if text[open..].contains('»') {
        None
    } else {
        Some(open)
    }
}

/// text の末尾に続く名前の先頭。再帰で書くと、長い入力でスタックを使い切る。
fn name_run(text: &str) -> usize {
    std::iter::successors(Some(text.len()), |&end| name_step(&text[..end]))
        .last()
        .unwrap_or(text.len())
}

/// text の末尾の 1 文字か `«…»` を名前の一部にしたときの、残りの長さ。
fn name_step(text: &str) -> Option<usize> {
    match text.chars().next_back() {
        Some('»') => text.rfind('«'),
        Some(c) if is_name_char(c) => Some(text.len() - c.len_utf8()),
        _ => None,
    }
}

/// 名前に使える文字。Lean の `isIdRest` (`Init/Meta/Defs.lean`) と同じ規則に `.` を
/// 足したもの。英数字は ASCII だけで、漢字や全角の英字は使えない。
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '_' | '\'' | '!' | '?' | '.')
        || is_letter_like(c)
        || is_subscript_alnum(c)
}

/// Lean の `isLetterLike`。ギリシャ文字 (λ、Π、Σ を除く)、ラテン文字の拡張、
/// `ℕ` などの記号の文字、数学用の英字。
fn is_letter_like(c: char) -> bool {
    matches!(c,
        '\u{3b1}'..='\u{3c9}' | '\u{391}'..='\u{3a9}' | '\u{3ca}'..='\u{3fb}'
        | '\u{1f00}'..='\u{1ffe}' | '\u{2100}'..='\u{214f}' | '\u{1d49c}'..='\u{1d59f}'
        | '\u{c0}'..='\u{ff}' | '\u{100}'..='\u{17f}')
        && !matches!(c, 'λ' | 'Π' | 'Σ' | '×' | '÷')
}

/// Lean の `isSubScriptAlnum`。下付きの数字と英字。
fn is_subscript_alnum(c: char) -> bool {
    matches!(c,
        '\u{2080}'..='\u{2089}' | '\u{2090}'..='\u{209c}' | '\u{1d62}'..='\u{1d6a}' | '\u{2c7c}')
}

/// ヘルプに書いてある `:` コマンドの名前 (`:` は付けない)。補完の候補にする。
///
/// ヘルプから取り出すので、ヘルプに載せたコマンドだけが候補になる。`:{` と `:!` は
/// 名前ではないので含めない。
pub fn meta_names(help_text: &str) -> Vec<String> {
    static META: LazyLock<Regex> = LazyLock::new(|| Regex::new(r":(\w+)").expect("正規表現"));
    // regex は後読み (`(?<![\w:])`) を使えないので、`:` の直前の文字を確かめる
    let after_word = |at: usize| {
        help_text[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ':')
    };
    META.captures_iter(help_text)
        .filter(|m| !after_word(m.get(0).map_or(0, |all| all.start())))
        .filter_map(|m| m.get(1).map(|name| name.as_str().to_string()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// 定数名をまとめて取得する単位。名前空間があれば最後の `.` まで、無ければ
/// 先頭 2 文字。
///
/// Mathlib では 1 文字にすると `C` だけで 7.5 万件 (3.4MB) になるので、単位を
/// 大きくしすぎない。`Nat.` なら 5684 件、`MeasureTheory.` でも 1 万件に収まる。
pub fn name_chunk(prefix: &str) -> &str {
    match prefix.rfind('.') {
        Some(dot) => &prefix[..=dot],
        None => {
            let end = prefix
                .char_indices()
                .nth(2)
                .map_or(prefix.len(), |(i, _)| i);
            &prefix[..end]
        }
    }
}

/// 補完のキャッシュを今の環境で使えるか。`at` はキャッシュを取得したときの
/// (世代, env)、`stack` は今の env の元になった環境。
///
/// 取得したときの env が今の env か `stack` にあれば、キャッシュの名前は今の環境にも
/// ある。そのあとに実行した宣言の名前は、呼び出し側が log から足す。`:undo` や
/// `:reset` で取り消した環境で取得したキャッシュは、取り消した宣言の名前を含むので
/// 使わない。
pub fn cache_usable(
    at: (u64, Option<i64>),
    generation: u64,
    env: Option<i64>,
    stack: &[i64],
) -> bool {
    let (made, cached) = at;
    made == generation && (cached == env || cached.is_some_and(|e| stack.contains(&e)))
}

/// `ns` を open したとき (`ns` が空なら root) に使える、prefix で始まる短い名前。
///
/// chunk は COMPLETE_QUERY の結果のチャンクの 1 つで、protected な名前は先頭に ! が
/// 付いている。
/// Lean と同じく protected な名前は最後の 1 語だけでは書けないので出さない
/// (`open Nat` しても `add_comm` は `Nat.add_comm` にならない)。
pub fn shorten<S: AsRef<str>>(chunk: &[S], prefix: &str, ns: &str, hidden: &[S]) -> Vec<String> {
    let head = if ns.is_empty() {
        String::new()
    } else {
        format!("{ns}.")
    };
    let want = format!("{head}{prefix}");
    chunk
        .iter()
        .map(AsRef::as_ref)
        .filter_map(|x| {
            let protected = x.starts_with('!');
            let name = x.strip_prefix('!').unwrap_or(x);
            name.starts_with(&want)
                .then(|| (protected, &name[head.len()..]))
        })
        .filter(|(protected, short)| {
            let unwritable = !head.is_empty() && *protected && !short.contains('.');
            let shadowed = hidden.iter().any(|h| h.as_ref() == *short);
            !unwritable && !shadowed
        })
        .map(|(_, short)| short.to_string())
        .collect()
}

/// カーソルの前の文字列 `text` から、Tab で何を補完するかを決める。
///
/// ふだんは定数名。`:` で始まる行ではコマンド名を補完し、そのあとはコマンドに
/// 合わせる。`:l` と `:save` の後はファイルのパス、`:env` の後は環境の名前、`:!` の後は
/// shell のコマンドかパス。カーソルの手前が `\name` の途中なら、行のどこでも略記の
/// 記号を補完する。space キーがどこでも略記を変換するので、Tab キーも同じ範囲で
/// 変換する。
pub fn completion_target(text: &str) -> Target {
    let (head, arg) = match text.find(' ') {
        Some(i) => (&text[..i], Some(&text[i + 1..])),
        None => (text, None),
    };
    match (abbrev_head(text), head, arg) {
        (Some(name), _, _) => Target::Abbrev {
            start: text.len() - name.len() - 1,
            symbols: abbrev_candidates(name)
                .into_iter()
                .map(String::from)
                .collect(),
        },
        (None, h, _) if h.starts_with(":!") => shell_target(text, 2),
        (None, h, None) if h.starts_with(':') => Target::Meta {
            start: 1,
            prefix: h[1..].to_string(),
        },
        (None, ":l" | ":load" | ":save", Some(a)) => Target::Path {
            start: text.len() - a.trim_start().len(),
            word: a.trim_start().to_string(),
        },
        (None, ":env", Some(a)) => Target::Env {
            start: text.len() - a.trim_start().len(),
            prefix: a.trim_start().to_string(),
        },
        _ => {
            let start = name_start(text);
            Target::Name {
                start,
                prefix: text[start..].to_string(),
            }
        }
    }
}

/// `:!` のあと (`text[from..]`) の補完。1 語目は PATH にあるコマンド、2 語目からは
/// ファイルのパス。1 語目でも `/` を含んでいれば `./run.sh` のようなパスとして扱う。
fn shell_target(text: &str, from: usize) -> Target {
    let rest = &text[from..];
    let (before, word) = match rest.rfind(' ') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => ("", rest),
    };
    Target::Shell {
        start: text.len() - word.len(),
        word: word.to_string(),
        command: before.trim().is_empty() && !word.contains('/'),
    }
}

/// words のうち prefix で始まるものを、順を変えずに返す。
pub fn pick<S: AsRef<str>>(words: &[S], prefix: &str) -> Vec<String> {
    words
        .iter()
        .map(AsRef::as_ref)
        .filter(|w| w.starts_with(prefix))
        .map(String::from)
        .collect()
}

/// `SCOPE_QUERY` の応答を読む。応答がオブジェクトでなければ空の `Scope`。
pub fn scope_of(got: Option<&Json>) -> Scope {
    let pair = |v: &Json| -> Option<(String, Vec<String>)> {
        let name = v.get(0)?.as_str()?.to_string();
        let rest = v
            .get(1)
            .and_then(Json::as_array)
            .map_or_else(Vec::new, |xs| {
                xs.iter()
                    .filter_map(Json::as_str)
                    .map(String::from)
                    .collect()
            });
        Some((name, rest))
    };
    match got {
        Some(v) if v.is_object() => Scope {
            opens: list(v, "open").iter().filter_map(pair).collect(),
            aliases: list(v, "alias")
                .iter()
                .filter_map(pair)
                .map(|(alias, _)| alias)
                .collect(),
        },
        _ => Scope::default(),
    }
}

/// REPL で実行した宣言の名前。キャッシュに無い名前も候補に出すため。
pub fn own_names<S: AsRef<str>>(log: &[S]) -> Vec<String> {
    log.iter()
        .flat_map(|src| {
            DECL_NAME
                .captures_iter(src.as_ref())
                .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
                .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// 名前空間 ns の中で prefix を完全な名前にしたもの。ns が空なら root。
fn full_name(ns: &str, prefix: &str) -> String {
    if ns.is_empty() {
        prefix.to_string()
    } else {
        format!("{ns}.{prefix}")
    }
}

/// root と open した名前空間それぞれで、prefix を完全な名前にしたもの。
/// `open Lean` のあとの `Json.pa` は `Lean.Json.pa` としても探す。
pub fn full_names(prefix: &str, opens: &[(String, Vec<String>)]) -> Vec<(String, String)> {
    std::iter::once(String::new())
        .chain(opens.iter().map(|(ns, _)| ns.clone()))
        .map(|ns| {
            let full = full_name(&ns, prefix);
            (ns, full)
        })
        .collect()
}

/// 今の環境で短い名前で書ける定数のうち、prefix で始まるもの。
///
/// chunks は `name_chunk` ごとの定数名 (COMPLETE_QUERY の応答)。root と open した
/// 名前空間からそれぞれ探して短くし、alias と REPL で実行した宣言の名前を足す。
/// 同じ名前空間を `hiding` を変えて何度も open できるので、隠す名前は open ごとに
/// 見る。
pub fn complete_hits<S: AsRef<str>>(
    prefix: &str,
    scope: &Scope,
    chunks: &BTreeMap<String, Vec<String>>,
    own: &[S],
) -> Vec<String> {
    let root = (String::new(), Vec::new());
    let opened = std::iter::once(&root)
        .chain(scope.opens.iter())
        .filter_map(|(ns, hidden)| {
            let full = full_name(ns, prefix);
            chunks.get(name_chunk(&full)).map(|got| (ns, hidden, got))
        })
        .flat_map(|(ns, hidden, got)| shorten(got, prefix, ns, hidden));
    let aliased = scope.aliases.iter().cloned();
    let mine = own.iter().map(|x| x.as_ref().to_string());
    opened
        .chain(aliased)
        .chain(mine)
        .filter(|name| name.starts_with(prefix))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

// ------------------------------------------------------------------ URL

/// URL のクエリに入れる形にする (Python の `urllib.parse.quote` と同じ)。
///
/// 英数字と `_.-~/` 以外を `%XX` にする。`?a + ?b` の `+` をそのまま入れると、
/// loogle 側では空白として解釈される。
pub fn url_quote(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' | b'/' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ------------------------------------------------------------------ メッセージ

pub fn messages(resp: &Json) -> &[Json] {
    list(resp, "messages")
}

pub fn errors(resp: &Json) -> Vec<&Json> {
    messages(resp)
        .iter()
        .filter(|m| text(m, "severity") == Some("error"))
        .collect()
}

pub fn has_error(resp: &Json) -> bool {
    !errors(resp).is_empty()
}

/// info メッセージだけを連結したもの。#eval の出力はここに含まれる。
pub fn info_text(resp: &Json) -> String {
    messages(resp)
        .iter()
        .filter(|m| text(m, "severity") == Some("info"))
        .map(|m| text(m, "data").unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn sorries(resp: &Json) -> &[Json] {
    list(resp, "sorries")
}

// ------------------------------------------------------------------ import

/// 行を読み終えたときのブロックコメントの深さ。Lean の `/- -/` は入れ子。
pub fn comment_depth(line: &str, depth: usize) -> usize {
    // `/` と `-` は ASCII なので、byte で読んでも文字の途中に一致することはない
    let bytes = line.as_bytes();
    let (mut i, mut depth) = (0, depth);
    while i + 1 < bytes.len() {
        match &bytes[i..i + 2] {
            b"/-" => {
                depth += 1;
                i += 2;
            }
            b"-/" if depth > 0 => {
                depth -= 1;
                i += 2;
            }
            _ => i += 1,
        }
    }
    depth
}

/// 行からコメントを取り除いた残り。行の中で閉じないブロックコメントがあれば None。
///
/// `import X /- c -/` や `/- c -/ import X` も import の行として読むため。
fn code_of(line: &str) -> Option<String> {
    // `comment_depth` と同じく、ASCII の 2 文字だけを見るので byte で読む
    let bytes = line.as_bytes();
    let (mut i, mut depth, mut code) = (0, 0, Vec::new());
    while i < bytes.len() {
        match (bytes.get(i..i + 2), depth) {
            (Some(b"--"), 0) => break,
            // `«A--B»` の中の `--` と `/-` はコメントではない。`«` と `»` は 2 byte
            (Some([0xC2, 0xAB]), 0) => {
                let end = line[i..].find('»').map_or(line.len(), |n| i + n + 2);
                code.extend_from_slice(&bytes[i..end]);
                i = end - 1;
            }
            (Some(b"/-"), _) => {
                depth += 1;
                i += 1;
            }
            (Some(b"-/"), 1..) => {
                depth -= 1;
                i += 1;
            }
            (_, 0) => code.push(bytes[i]),
            (_, _) => {}
        }
        i += 1;
    }
    match depth {
        0 => Some(String::from_utf8_lossy(&code).trim().to_string()),
        _ => None,
    }
}

/// 先頭のヘッダ領域にある `module` と import を、行番号と一緒に返す。
///
/// ヘッダ領域は、空行、コメント、`module`、import が続く範囲。行の中で閉じるコメントは
/// 取り除いてから判定し、import の行はコメントを除いた形で返す。行単位で `import` を
/// 探すと、doc コメントに書いた `import Foo` を本物の import と取り違える。
/// それを :save のヘッダに書き出すと、repl はヘッダ全体を無視して起動するので、
/// 書き出したファイルは :l でも lean でもエラーになる。import 行を取り除く処理
/// でも同じで、コメントの行を消してコメントをおかしくする。
///
/// Lean は `module` を import より前にしか認めないが、import の後ろにあっても
/// ヘッダ領域に含める。ヘッダ領域を終わらせると、取り除いたあとの先頭に `module`
/// が残る。
pub fn scan_header(text: &str) -> (Header, BTreeSet<usize>) {
    let mut head = Header::default();
    let mut at = BTreeSet::new();
    let mut depth = 0;

    for (n, line) in text.lines().enumerate() {
        let one = line.trim();
        match (depth, code_of(one).as_deref()) {
            (1.., _) => depth = comment_depth(line, depth),
            (0, None) if one.starts_with("/-") => depth = comment_depth(line, 0),
            (0, Some("")) => {}
            (0, Some(code)) if is_module_line(code) => {
                head.module = true;
                at.insert(n);
            }
            (0, Some(code)) if import_of(code).is_some() => {
                head.imports.push(code.to_string());
                at.insert(n);
            }
            _ => break, // ヘッダ領域はここで終わり。以降の import は Lean も認めない
        }
    }

    (head, at)
}

/// `module` の行か。後ろに行コメントを書いてもよい。前後の空白は取り除いてある
/// 行を受け取る。
fn is_module_line(one: &str) -> bool {
    one == "module" || drop_word(one, "module").is_some_and(|rest| rest.starts_with("--"))
}

/// import の行ならモジュール名。コメントと前後の空白は取り除いてある行を受け取る。
///
/// module のファイルでは、`public`、`meta`、`all` を付けて書ける
/// (`public meta import all X`)。
fn import_of(one: &str) -> Option<&str> {
    let rest = drop_word(one, "public").unwrap_or(one);
    let rest = drop_word(rest, "meta").unwrap_or(rest);
    let rest = drop_word(rest, "import")?;
    Some(drop_word(rest, "all").unwrap_or(rest).trim())
}

/// 先頭の語が `word` なら、その後ろ。
fn drop_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    match text.split_once(char::is_whitespace) {
        Some((first, rest)) if first == word => Some(rest.trim_start()),
        _ => None,
    }
}

/// import しているモジュールの名前を並べる。
pub fn import_lines(text: &str) -> Vec<String> {
    scan_header(text)
        .0
        .imports
        .iter()
        .filter_map(|line| import_of(line))
        .map(String::from)
        .collect()
}

/// `module` と import の行を取り除く。既存の環境の上で実行するときに使う。
///
/// どちらもファイルの先頭にしか書けない。
pub fn strip_imports(text: &str) -> String {
    drop_lines(text, &scan_header(text).1)
}

fn drop_lines(text: &str, at: &BTreeSet<usize>) -> String {
    text.lines()
        .enumerate()
        .filter(|(n, _)| !at.contains(n))
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// module のファイルのヘッダ。完結判定と補完のクエリが使う Lean を足す。
///
/// module の中では、`import Lean` だけでは `elab` を書いたファイルや init が
/// "imported privately" で失敗するので、`public import Lean` にする。
pub fn module_header(imports: &[String]) -> String {
    let own = imports
        .iter()
        .filter(|line| line.as_str() != MODULE_PROBE)
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    format!("module\n{MODULE_PROBE}\n{own}")
}

const MODULE_PROBE: &str = "public import Lean";

/// `:l` で送るソース。`probe` は Lean の import、`imports` は設定の import の行。
///
/// module のファイルには、`module` の行、`public import Lean`、ファイルの import の
/// 順に並べ、設定の import は足さない。module から module でないファイルを
/// import すると、Lean は "cannot import non-`module`" で止まる。module でない
/// ファイルは、先頭に足すだけにする。ファイルが import を書いていれば、設定の
/// import は足さない。
pub fn with_header(raw: &str, probe: &str, imports: &str) -> String {
    let (head, at) = scan_header(raw);
    match (head.module, head.imports.is_empty()) {
        (true, _) => format!("{}{}", module_header(&head.imports), drop_lines(raw, &at)),
        (false, false) => format!("{probe}{raw}"),
        (false, true) => format!("{probe}{imports}{raw}"),
    }
}

/// `:save` が書き出すヘッダ。`imports` は設定の import、`loaded` は `:l` した
/// ファイルのヘッダ。
///
/// `:l` したファイルの import を足さないと、そのファイルが import していた
/// モジュールがヘッダに含まれなくなる。その場合 `:save` は「宣言 n 件を書き出した」
/// と成功を報告するのに、書き出したファイルは `:l` でも lean でもエラーになる
/// (Unknown identifier が並ぶ)。module のファイルを `:l` していたら、`module` と
/// その import をそのまま書き、設定の import は足さない (`with_header`)。
pub fn save_header(imports: &[String], loaded: &Header) -> String {
    if loaded.module {
        module_header(&loaded.imports)
    } else {
        let own = loaded.imports.iter().filter_map(|line| import_of(line));
        let mods: Vec<&str> = std::iter::once("Lean")
            .chain(imports.iter().map(String::as_str))
            .chain(own)
            .collect();
        // 同じモジュールは、最初に出てきたところにだけ書く。
        mods.iter()
            .enumerate()
            .filter(|&(n, m)| !mods[..n].contains(m))
            .map(|(_, m)| format!("import {m}\n"))
            .collect()
    }
}

/// ファイルが自分で import を書いているか。コメントの中のものは数えない。
pub fn has_import(src: &str) -> bool {
    !import_lines(src).is_empty()
}

// ------------------------------------------------------------------ 表示

/// 長い 1 行を端末の幅に収める。折り返すと、並べたときに行の対応がずれる。
/// 幅は文字の数で数える。
pub fn clip(line: &str, width: usize) -> String {
    if line.chars().count() <= width {
        line.to_string()
    } else {
        let head: String = line.chars().take(width.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

/// 宣言 1 件を 1 行で表す。報告に使うので、長ければ切り詰める。
pub fn head_line(src: &str, width: usize) -> String {
    clip(src.trim().split('\n').next().unwrap_or(""), width)
}

/// エラーメッセージを連結したもの。長ければ先頭の keep 行だけ。
pub fn error_text(resp: &Json, keep: usize) -> String {
    let blob = errors(resp)
        .iter()
        .map(|m| text(m, "data").unwrap_or("").trim())
        .collect::<Vec<_>>()
        .join("\n");
    blob.trim()
        .split('\n')
        .take(keep)
        .collect::<Vec<_>>()
        .join("\n")
}

// ------------------------------------------------------------------ sorry の置き換え

/// repl の位置 {"line": 1 始まり, "column": 0 始まり} を src の byte の位置に変換する。
/// column はコードポイント単位で数える。
pub fn offset(src: &str, pos: Option<&Json>) -> Option<usize> {
    let pos = pos?;
    let (line, col) = (pos.get("line")?.as_u64()?, pos.get("column")?.as_u64()?);
    let line = usize::try_from(line).ok()?;
    let col = usize::try_from(col).ok()?;
    let lines: Vec<&str> = src.split('\n').collect();
    let one = lines.get(line.checked_sub(1)?)?;
    let start: usize = lines[..line - 1].iter().map(|l| l.len() + 1).sum();
    let within = one
        .char_indices()
        .map(|(i, _)| i)
        .chain([one.len()])
        .nth(col)?;
    Some(start + within)
}

/// sorry 1 つをタクティクのスクリプトに置き換える。位置が取得できなければ None。
///
/// repl は sorry ごとに pos / endPos を返す。テキストを検索して sorry を探すと、
/// コメントや識別子の中の "sorry" にも一致するので、必ず位置で切り出す。
/// 位置で切り出せば、sorry が 2 つ以上あっても 1 つだけを置き換えられる。
pub fn splice_sorry(src: &str, sy: &Json, script: &str) -> Option<String> {
    let a = offset(src, sy.get("pos"))?;
    let b = offset(src, sy.get("endPos"))?;
    if src.get(a..b) == Some("sorry") {
        Some(put_script(src, a, b, script))
    } else {
        None
    }
}

/// src[a..b] の sorry をスクリプトに置き換える。
pub fn put_script(src: &str, a: usize, b: usize, script: &str) -> String {
    let pad = &src[src[..a].rfind('\n').map_or(0, |i| i + 1)..a]; // sorry の行の、sorry までの部分
    if !script.contains('\n') {
        // 1 行なら sorry のあった桁にそのまま置く。前後の空白は変えない。
        // 変えると ⟨sorry, …⟩ が ⟨ rfl, …⟩ になり、行頭にある sorry は
        // インデントが 1 桁になって by ブロックの外に出てしまう。
        format!("{}{script}{}", &src[..a], &src[b..])
    } else if pad.trim().is_empty() {
        // sorry だけの行。sorry の桁にスクリプトをそろえる。
        format!("{}{}{}", &src[..a], hang(script, pad), &src[b..])
    } else {
        // 行の途中 (:= by sorry)。次の行に、その行より 2 桁深くインデントして置く。
        let indent = &pad[..pad.len() - pad.trim_start_matches([' ', '\t']).len()];
        let deeper = format!("{indent}  ");
        format!(
            "{}\n{deeper}{}{}",
            src[..a].trim_end_matches([' ', '\t']),
            hang(script, &deeper),
            &src[b..]
        )
    }
}

/// 空白だけの行を除くすべての行の先頭に pad を付ける (Python の `textwrap.indent`)。
pub fn indent(text: &str, pad: &str) -> String {
    text.split('\n')
        .map(|l| {
            if l.trim().is_empty() {
                l.to_string()
            } else {
                format!("{pad}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// スクリプトの 2 行目以降を pad の桁にそろえる。1 行目は呼び出し側が置く。
/// 文字列の中で始まる行には空白を足さない (`indent_code` と同じ)。
pub fn hang(script: &str, pad: &str) -> String {
    pad_lines(script, pad, 1)
}

/// Lean のコードの、空白だけの行と文字列の中で始まる行を除くすべての行の先頭に
/// pad を付ける。
///
/// 複数行の文字列の 2 行目以降に空白を足すと、文字列の値が変わる。コードを
/// `#eval` などに包むときと、sorry をスクリプトに置き換えるときに使う。
pub fn indent_code(text: &str, pad: &str) -> String {
    pad_lines(text, pad, 0)
}

/// from 行目以降の、空白だけの行と文字列の中で始まる行を除く行に pad を付ける。
fn pad_lines(text: &str, pad: &str, from: usize) -> String {
    text.split('\n')
        .zip(outside_strings(text))
        .enumerate()
        .map(|(n, (one, outside))| {
            if n >= from && outside && !one.trim().is_empty() {
                format!("{pad}{one}")
            } else {
                one.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 字句を読んでいる途中で、まだ閉じていない文字列、コメント、`{`。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Open {
    /// 文字列。`interp` は `s!` などの後ろの文字列か。そうなら `{}` の中はコードになる
    Str { interp: bool },
    /// raw 文字列。`#` の数を持つ
    Raw(usize),
    /// ブロックコメント。入れ子にできる
    Comment,
    /// コードの中の `{`
    Brace,
}

/// Lean のコードの各行 (`\n` で区切る) が、文字列の外で始まるか。
///
/// 文字列の外で始まる行なら、先頭に空白を足してもコードの意味は変わらない。
/// ブロックコメントの中で始まる行も、空白を足してよいので true にする。
/// コメントの中と `'"'` の引用符、`«»` の中の引用符では文字列は始まらない。
pub fn outside_strings(text: &str) -> Vec<bool> {
    let cs: Vec<char> = text.chars().collect();
    let (mut i, mut stack, mut out) = (0, Vec::new(), vec![true]);
    while i < cs.len() {
        if cs[i] == '\n' {
            out.push(!matches!(
                stack.last(),
                Some(Open::Str { .. } | Open::Raw(_))
            ));
            i += 1;
        } else {
            i = lex_step(&cs, i, &mut stack);
        }
    }
    out
}

/// cs[i] から 1 つの字句を読み、開いたものと閉じたものを stack に反映する。
/// 返すのは次に読む位置。`\n` は読まずに手前で止まる。
fn lex_step(cs: &[char], i: usize, stack: &mut Vec<Open>) -> usize {
    match stack.last().copied() {
        Some(Open::Str { interp }) => str_step(cs, i, stack, interp),
        Some(Open::Raw(hashes)) => raw_step(cs, i, stack, hashes),
        Some(Open::Comment) => comment_step(cs, i, stack),
        Some(Open::Brace) | None => code_step(cs, i, stack),
    }
}

fn str_step(cs: &[char], i: usize, stack: &mut Vec<Open>, interp: bool) -> usize {
    match (cs[i], cs.get(i + 1)) {
        // `\` は次の 1 文字と一緒に読む。行末の `\` の次の行は、文字列の中で始まる行にする
        ('\\', Some(&next)) if next != '\n' => i + 2,
        ('"', _) => {
            stack.pop();
            i + 1
        }
        ('{', _) if interp => {
            stack.push(Open::Brace);
            i + 1
        }
        _ => i + 1,
    }
}

fn raw_step(cs: &[char], i: usize, stack: &mut Vec<Open>, hashes: usize) -> usize {
    let closes = cs[i] == '"' && (1..=hashes).all(|k| cs.get(i + k) == Some(&'#'));
    if closes {
        stack.pop();
        i + 1 + hashes
    } else {
        i + 1
    }
}

fn comment_step(cs: &[char], i: usize, stack: &mut Vec<Open>) -> usize {
    match (cs[i], cs.get(i + 1)) {
        ('/', Some('-')) => {
            stack.push(Open::Comment);
            i + 2
        }
        ('-', Some('/')) => {
            stack.pop();
            i + 2
        }
        _ => i + 1,
    }
}

fn code_step(cs: &[char], i: usize, stack: &mut Vec<Open>) -> usize {
    let after_name = i > 0 && name_char(cs[i - 1]);
    match (cs[i], cs.get(i + 1)) {
        ('-', Some('-')) => cs[i..]
            .iter()
            .position(|&c| c == '\n')
            .map_or(cs.len(), |n| i + n),
        ('/', Some('-')) => {
            stack.push(Open::Comment);
            i + 2
        }
        ('"', _) => {
            stack.push(Open::Str {
                interp: i > 0 && cs[i - 1] == '!',
            });
            i + 1
        }
        ('r', Some('"' | '#')) if !after_name => raw_open(cs, i, stack),
        ('\'', Some(_)) if !after_name => char_end(cs, i),
        ('«', _) => escape_end(cs, i),
        ('{', _) => {
            stack.push(Open::Brace);
            i + 1
        }
        ('}', _) if stack.last() == Some(&Open::Brace) => {
            stack.pop();
            i + 1
        }
        _ => i + 1,
    }
}

/// cs[i] の `r` が `r"` か `r#"` の始まりなら raw 文字列を開く。
fn raw_open(cs: &[char], i: usize, stack: &mut Vec<Open>) -> usize {
    let hashes = cs[i + 1..].iter().take_while(|&&c| c == '#').count();
    if cs.get(i + 1 + hashes) == Some(&'"') {
        stack.push(Open::Raw(hashes));
        i + 2 + hashes
    } else {
        i + 1
    }
}

/// cs[i] の `'` が文字のリテラル (`'a'`、`'\n'`) の始まりなら、その後ろの位置。
/// そうでなければ `'` の次の位置。
fn char_end(cs: &[char], i: usize) -> usize {
    let close = match cs.get(i + 1) {
        Some('\\') => cs[i + 2..]
            .iter()
            .take_while(|&&c| c != '\n')
            .position(|&c| c == '\'')
            .map(|n| i + 2 + n),
        Some('\n') => None,
        _ => Some(i + 2).filter(|&k| cs.get(k) == Some(&'\'')),
    };
    close.map_or(i + 1, |k| k + 1)
}

/// cs[i] の `«` で始まる名前の後ろの位置。行の中で閉じなければ行末。
fn escape_end(cs: &[char], i: usize) -> usize {
    let k = i
        + 1
        + cs[i + 1..]
            .iter()
            .take_while(|&&c| c != '»' && c != '\n')
            .count();
    if cs.get(k) == Some(&'»') { k + 1 } else { k }
}

/// 名前に使える文字か。`h'` の `'` や `s!` の `!` も名前の一部になる。
fn name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '\'' | '!' | '?')
}

/// `:save` が書き出す本文。環境を構成しているソースに、保留中の宣言をコメントとして
/// 足す。
///
/// 保留中の宣言 (環境に追加されなかったもの) を省くと、`:save` は成功を報告したのに
/// 入力した宣言が消える。コメントにせずそのまま書くと、lean でエラーになるファイル
/// になる。
pub fn save_body(srcs: &[String], orphans: &[String]) -> String {
    let mut parts: Vec<String> = srcs.to_vec();
    if !orphans.is_empty() {
        let note = orphans
            .iter()
            .map(|one| indent(one, "-- "))
            .collect::<Vec<_>>()
            .join("\n\n");
        parts.push(format!("-- 保留中の宣言 ({} 件):\n{note}", orphans.len()));
    }
    parts.join("\n\n")
}

// ------------------------------------------------------------------ 応答の切り出し

/// 受信バッファから最初の完全なレスポンスを取り出す。まだすべて届いていなければ None。
///
/// レスポンスは空行で終わる。JSON の中に空行が含まれることもあるので、空行の位置を
/// 順に試して、最初にパースできたものを使う。
pub fn first_response(buf: &str) -> Option<Json> {
    static BLANK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n[ \t]*\n").expect("正規表現"));
    BLANK
        .find_iter(buf)
        .find_map(|m| as_response(&buf[..m.start()]))
}

/// text をレスポンスとしてパースする。パースできなければ None。
///
/// repl の応答は必ずオブジェクト。配列や数値ならパースできなかったものとして扱い、
/// 次の区切りを試す (呼び出し側はキーで値を取り出す)。
pub fn as_response(text: &str) -> Option<Json> {
    serde_json::from_str::<Json>(text)
        .ok()
        .filter(Json::is_object)
}

/// エンジンの出力の最後の行を JSON としてパースする。出力が空か、最後の行が
/// JSON でなければ None。問い合わせは結果を最後の行に出力する。
pub fn last_json(out: Option<&str>) -> Option<Json> {
    let last = out.unwrap_or("").trim().lines().next_back()?;
    serde_json::from_str(last).ok()
}

/// `KEY=value` の並びを表にする。lake env のキャッシュを読むため。
pub fn parse_env_lines(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// シグナル番号を名前にする。知らない番号は `signal N` にする。
pub fn signal_name(num: i32) -> String {
    match SIGNALS.iter().find(|(n, _)| *n == num) {
        Some((_, name)) => (*name).to_string(),
        None => format!("signal {num}"),
    }
}

// Python の `signal.Signals(num).name` と同じ名前。番号は OS ごとに違う。
#[cfg(target_os = "macos")]
const SIGNALS: &[(i32, &str)] = &[
    (1, "SIGHUP"),
    (2, "SIGINT"),
    (3, "SIGQUIT"),
    (4, "SIGILL"),
    (5, "SIGTRAP"),
    (6, "SIGABRT"),
    (7, "SIGEMT"),
    (8, "SIGFPE"),
    (9, "SIGKILL"),
    (10, "SIGBUS"),
    (11, "SIGSEGV"),
    (12, "SIGSYS"),
    (13, "SIGPIPE"),
    (14, "SIGALRM"),
    (15, "SIGTERM"),
    (16, "SIGURG"),
    (17, "SIGSTOP"),
    (18, "SIGTSTP"),
    (19, "SIGCONT"),
    (20, "SIGCHLD"),
    (21, "SIGTTIN"),
    (22, "SIGTTOU"),
    (23, "SIGIO"),
    (24, "SIGXCPU"),
    (25, "SIGXFSZ"),
    (26, "SIGVTALRM"),
    (27, "SIGPROF"),
    (28, "SIGWINCH"),
    (29, "SIGINFO"),
    (30, "SIGUSR1"),
    (31, "SIGUSR2"),
];

#[cfg(not(target_os = "macos"))]
const SIGNALS: &[(i32, &str)] = &[
    (1, "SIGHUP"),
    (2, "SIGINT"),
    (3, "SIGQUIT"),
    (4, "SIGILL"),
    (5, "SIGTRAP"),
    (6, "SIGABRT"),
    (7, "SIGBUS"),
    (8, "SIGFPE"),
    (9, "SIGKILL"),
    (10, "SIGUSR1"),
    (11, "SIGSEGV"),
    (12, "SIGUSR2"),
    (13, "SIGPIPE"),
    (14, "SIGALRM"),
    (15, "SIGTERM"),
    (16, "SIGSTKFLT"),
    (17, "SIGCHLD"),
    (18, "SIGCONT"),
    (19, "SIGSTOP"),
    (20, "SIGTSTP"),
    (21, "SIGTTIN"),
    (22, "SIGTTOU"),
    (23, "SIGURG"),
    (24, "SIGXCPU"),
    (25, "SIGXFSZ"),
    (26, "SIGVTALRM"),
    (27, "SIGPROF"),
    (28, "SIGWINCH"),
    (29, "SIGIO"),
    (30, "SIGPWR"),
    (31, "SIGSYS"),
    (34, "SIGRTMIN"),
    (64, "SIGRTMAX"),
];

// ------------------------------------------------------------------ エンジンのバージョン

static VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^v(\d+)\.(\d+)\.(\d+)(?:-rc(\d+))?$").expect("正規表現"));

/// `leanprover/lean4:v4.34.0-rc2` から `v4.34.0-rc2` を取る。
pub fn toolchain_version(tc: &str) -> &str {
    match tc.rsplit_once(':') {
        Some((_, "")) | None => tc,
        Some((_, version)) => version,
    }
}

static LEAN_VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"version (\d+\.\d+\.\d+)(-[^,\s]+)?").expect("正規表現"));

static RC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(-rc\d+)?$").expect("正規表現"));

/// `lean --version` の出力から toolchain を作る。
///
/// リリースと rc は `leanprover/lean4:v4.24.0-rc1`、nightly は
/// `leanprover/lean4:nightly-2025-10-01` にする。nightly の `4.24.0-nightly-…` を
/// `v4.24.0` と読むと、別のバージョンのエンジンをビルドする。ほかの接尾辞は
/// どの toolchain か分からないので None を返す。
pub fn toolchain_of_version(out: &str) -> Option<String> {
    let m = LEAN_VERSION.captures(out)?;
    let suffix = m.get(2).map_or("", |s| s.as_str());
    match (suffix.strip_prefix("-nightly-"), RC.is_match(suffix)) {
        (Some(date), _) => Some(format!("leanprover/lean4:nightly-{date}")),
        (None, true) => Some(format!("leanprover/lean4:v{}{suffix}", &m[1])),
        (None, false) => None,
    }
}

/// elan のチャンネル (`stable`、`beta`、`nightly`) か。チャンネルが指すバージョンは
/// 新しいリリースが出ると変わる。
pub fn is_channel(tc: &str) -> bool {
    matches!(toolchain_version(tc), "stable" | "beta" | "nightly")
}

/// toolchain の書き方をそろえる。elan は配布元を省いた `v4.34.0` を
/// `leanprover/lean4:v4.34.0` と同じ toolchain として扱う。
pub fn canonical_toolchain(tc: &str) -> String {
    if tc.contains(':') {
        tc.to_string()
    } else {
        format!("leanprover/lean4:{tc}")
    }
}

/// 2 つの toolchain が、書き方を除いて同じか。
pub fn same_toolchain(a: &str, b: &str) -> bool {
    canonical_toolchain(a) == canonical_toolchain(b)
}

/// 自分でビルドしたエンジンの toolchain が、使うバージョンと違うときの警告の文。
///
/// leani が用意したエンジン (own が false) は使うバージョンでビルドしてあるので、
/// バージョンは必ず一致する。自分でビルドしたエンジンだけ、バージョンが違って
/// いたら警告する (leani がビルドし直すことはしない)。engine_tc が空なら、
/// エンジンの lean-toolchain が読めなかったので比べない。
pub fn toolchain_warning(own: bool, engine_tc: &str, tc: &str, engine_dir: &str) -> Option<String> {
    if own && !engine_tc.is_empty() && !same_toolchain(engine_tc, tc) {
        Some(format!(
            "警告: toolchain が違う (使うバージョン={tc} / エンジン={engine_tc})。\n  cd {engine_dir} && lake build repl"
        ))
    } else {
        None
    }
}

/// バージョンを比較するためのキー。rc は同じバージョンの正式リリースより前になる。
///
/// 解釈できない形なら None。
pub fn version_key(tag: &str) -> Option<(u64, u64, u64, u64)> {
    let m = VERSION.captures(tag)?;
    let num = |i: usize| m.get(i).and_then(|g| g.as_str().parse::<u64>().ok());
    Some((num(1)?, num(2)?, num(3)?, num(4).unwrap_or(1 << 30)))
}

/// そのバージョンに使う repl のタグ。同名のタグがあればそれ、無ければそのバージョン
/// 以下で一番新しいもの。
///
/// repl のタグは Lean のバージョンと同名だが、patch リリースには付かないことがある
/// (v4.33.0 はあるが v4.33.1 は無い)。patch で API は変わらないので、1 つ前の
/// タグのソースを目的のバージョンでビルドすれば、ビルドに成功する。
pub fn pick_tag<S: AsRef<str>>(version: &str, tags: &[S]) -> Option<String> {
    let want = version_key(version)?;
    if tags.iter().any(|t| t.as_ref() == version) {
        Some(version.to_string())
    } else {
        tags.iter()
            .map(AsRef::as_ref)
            .filter_map(|t| version_key(t).filter(|k| *k <= want).map(|k| (k, t)))
            .max()
            .map(|(_, t)| t.to_string())
    }
}

/// エンジンのパス。設定で明示されていればそれ、無ければ cache の下のバージョンごとのパス。
pub fn engine_dir(engine: Option<&str>, tc: &str, cache: &str) -> Result<String, EngineError> {
    match engine {
        Some(path) => Ok(path.to_string()),
        None => Ok(format!("{cache}/{}", version_dir(toolchain_version(tc))?)),
    }
}

/// バージョンごとのディレクトリの名前。
///
/// バージョン名をそのままディレクトリ名に使うので、パスの区切り文字などは `-` に
/// 置き換える。さらに build_engine はこのディレクトリを作り直す (中身ごと消す)
/// ので、`..` のように親ディレクトリを指す名前は受け付けない。
pub fn version_dir(version: &str) -> Result<String, EngineError> {
    let name: String = version
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if name.trim_matches(['.', '-']).is_empty() {
        Err(EngineError(format!(
            "バージョンの名前として使えない: {version}"
        )))
    } else {
        Ok(name)
    }
}

// ------------------------------------------------------------------ 完結判定

/// パーサがエラーになるまでに何行何桁まで読めたか。先まで読めたほうを、ユーザーが意図した種類とみなす。
pub fn err_pos(msg: &str) -> (u64, u64) {
    let num = |s: &str| s.parse().unwrap_or(0);
    match ERR_POS.captures(msg) {
        Some(m) => (num(&m[1]), num(&m[2])),
        None => (0, 0),
    }
}

pub fn indented(line: &str) -> bool {
    line.starts_with([' ', '\t'])
}

/// この行が単独の入力になりえないか。インデント行と、行頭の `|`。
/// `|` で始まる command も tactic も Lean には無いので、必ず前の行の続き。
pub fn continues(line: &str) -> bool {
    indented(line) || line.trim_start().starts_with('|')
}

/// 完結しているが、続きの行がありうるので空行を待つべきか。
pub fn block_continues<S: AsRef<str>>(buf: &[S], src: &str) -> bool {
    let last_continues = match buf {
        [_, .., last] => continues(last.as_ref()),
        _ => false,
    };
    last_continues || BLOCK_OPEN.is_match(src)
}

fn parsed(probe: &Json, cat: &str) -> bool {
    probe.get(cat).and_then(|p| p.get("ok")) == Some(&Json::Bool(true))
}

fn parse_err<'a>(probe: &'a Json, cat: &str) -> &'a str {
    probe.get(cat).and_then(|p| text(p, "err")).unwrap_or("")
}

/// パーサの応答を (入力の状態, 送り方) に変換する。
///
/// 判定できなければ Err にし、入力をそのまま送って Lean に本当のエラーを出させる
/// (leani の推測でエラーを作らない)。
pub fn classify(probe: Option<&Json>) -> (State, Kind) {
    match probe {
        None => (State::Err, Kind::Cmd),
        Some(p) if parsed(p, "cmd") => (State::Complete, Kind::Cmd),
        Some(p) if parsed(p, "term") => (State::Complete, Kind::Term),
        Some(p) => {
            let (cmd_err, term_err) = (parse_err(p, "cmd"), parse_err(p, "term"));
            let kind = if err_pos(cmd_err) >= err_pos(term_err) {
                Kind::Cmd
            } else {
                Kind::Term
            };
            if INCOMPLETE.is_match(cmd_err) || INCOMPLETE.is_match(term_err) {
                (State::More, kind)
            } else {
                (State::Err, kind)
            }
        }
    }
}

/// 証明モードでは、タクティクの列として読めるかだけを見る。
pub fn classify_tac(probe: Option<&Json>) -> (State, Kind) {
    let err = probe
        .and_then(|p| p.get("tac"))
        .and_then(|t| text(t, "err"));
    match (probe, err) {
        (Some(p), _) if parsed(p, "tac") => (State::Complete, Kind::Tac),
        (_, Some(err)) if INCOMPLETE.is_match(err) => (State::More, Kind::Tac),
        _ => (State::Err, Kind::Tac),
    }
}

// ------------------------------------------------------------------ 証明モード

/// `exact?` や `simp?` は結果を "Try this:" として info で返す。スクリプトには
/// この提案の中身を使う。`exact?` のままだと、:save したファイルを読み込むたびに
/// 検索が実行され、結果も環境によって変わる。提案の先頭には `[apply]` のような
/// ラベルが付く。
static SUGGESTION_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[[^\]]*\]\s*").expect("正規表現"));

/// 提案を最後まで読み取れたかの目安。折り返した行を読み損ねると、必ず括弧の対応が
/// 合わなくなる。
pub const PAIRS: [(char, char); 4] = [('(', ')'), ('[', ']'), ('{', '}'), ('⟨', '⟩')];

/// タクティクを 1 つ進めた Proof。前の proofState とゴールは :undo 用に積む。
pub fn tactic_step(proof: &Proof, state: i64, goals: &[String], tactic: &str) -> Proof {
    let mut script = proof.script.clone();
    script.push(tactic.to_string());
    let mut stack = proof.stack.clone();
    stack.push((proof.state, proof.goals.clone()));
    Proof {
        state,
        goals: goals.to_vec(),
        generation: proof.generation,
        script,
        stack,
    }
}

pub fn tactic_undo(proof: &Proof) -> Option<Proof> {
    proof
        .stack
        .split_last()
        .map(|((state, goals), rest)| Proof {
            state: *state,
            goals: goals.clone(),
            generation: proof.generation,
            script: proof.script[..proof.script.len().saturating_sub(1)].to_vec(),
            stack: rest.to_vec(),
        })
}

/// エンジンに送るタクティク。REPL のタクティクモードはタクティクを 1 つしか読まないので、
/// `( )` で囲み、`;` で並べたタクティクも 1 つのタクティクとして送る。括弧は別の行に
/// 置く。同じ行に置くと、行コメントで終わるタクティクでは `)` がコメントの中に入る。
pub fn grouped_tactic(src: &str) -> String {
    format!("(\n{src}\n)")
}

/// `grouped_tactic` で送ったタクティクのパースエラーを、括弧で囲む前の入力に合わせる。
///
/// エンジンは括弧で囲んだものを読むので、`expected ')'` や閉じ括弧の行の位置を返す。
/// 完結判定 (`probe` の `tac`) は括弧で囲まずに読んでいるので、そのエラーがあれば
/// エンジンのエラーと置き換える。完結判定は各行を 2 文字字下げして `by` の次の行から
/// 置いて読むので、行を 1 つ、桁を 2 つ戻す。完結判定のエラーが無ければ、エンジンの
/// エラーの行を `(` の行の分だけ戻す。
pub fn tactic_parse_error(msg: &str, probe: Option<&Json>) -> String {
    let probed = probe
        .and_then(|p| p.get("tac"))
        .and_then(|t| text(t, "err"));
    match (ERR_POS.find(msg), probed) {
        (Some(at), Some(err)) => format!("{}{}", &msg[..at.start()], shift_pos(err, 1, 2)),
        _ => shift_pos(msg, 1, 0),
    }
}

/// メッセージの中の位置を、行は `lines`、桁は `cols` だけ戻す。行は 1 より前にしない。
fn shift_pos(msg: &str, lines: u64, cols: u64) -> String {
    ERR_POS
        .replace_all(msg, |m: &regex::Captures| {
            let num = |s: &str| s.parse::<u64>().unwrap_or(0);
            let line = num(&m[1]).saturating_sub(lines).max(1);
            let col = num(&m[2]).saturating_sub(cols);
            format!("<input>:{line}:{col}:")
        })
        .into_owned()
}

/// エンジンが PANIC を出力していたら、PANIC の行。無ければ None。
///
/// 同じメッセージの中で、PANIC の前に #eval の出力が、後にバックトレースが続く
/// ことがあるので、メッセージの 1 行目ではなく PANIC の行を探す。
pub fn panic_line(resp: &Json) -> Option<String> {
    messages(resp)
        .iter()
        .flat_map(|m| text(m, "data").unwrap_or("").lines())
        .find(|ln| ln.contains("PANIC at"))
        .map(str::to_string)
}

/// 括弧が閉じているか。提案を途中で切っていないかの確認に使う。
pub fn balanced(src: &str) -> bool {
    // 開く括弧は閉じる括弧を積み、閉じる括弧は積んだものと合わなければ None にする。
    let left = src.chars().try_fold(Vec::new(), |mut stack, ch| {
        let opens = PAIRS.iter().find(|(open, _)| *open == ch);
        let closes = PAIRS.iter().any(|(_, close)| *close == ch);
        match (opens, closes) {
            (Some(&(_, close)), _) => {
                stack.push(close);
                Some(stack)
            }
            (None, true) => (stack.pop() == Some(ch)).then_some(stack),
            (None, false) => Some(stack),
        }
    });
    left.is_some_and(|stack| stack.is_empty())
}

/// "Try this:" の提案を返す。無ければ None。
///
/// 提案は pretty printer が 100 桁前後で折り返すので、`simp?` の結果は
/// 普通に複数行になる。1 行目だけを取ると `simp only [a, b,` のような括弧の
/// 閉じていないスクリプトになり、しかもそれが「完成した証明」として表示される
/// ので、ユーザーは気付けない。改行を含めて返す (Lean のタクティクは複数行でも
/// 問題ない)。
pub fn try_this(messages: &[Json]) -> Option<String> {
    let data = messages
        .iter()
        .map(|m| text(m, "data").unwrap_or("").trim())
        .find(|d| d.starts_with("Try this:"))?;
    let body = dedent(data["Try this:".len()..].trim_matches('\n'));
    let found = SUGGESTION_TAG.replace(body.trim(), "");
    let found = found.trim();
    if found.is_empty() {
        None
    } else {
        Some(found.to_string())
    }
}

/// Python の `textwrap.dedent`。空白だけの行は空にし、ほかの行に共通する先頭の
/// 空白 (space と tab) を取り除く。
fn dedent(text: &str) -> String {
    let blank = |l: &str| l.trim_matches([' ', '\t']).is_empty();
    let indent = |l: &str| l.len() - l.trim_start_matches([' ', '\t']).len();
    let lines: Vec<&str> = text
        .split('\n')
        .map(|l| if blank(l) { "" } else { l })
        .collect();
    let margin = lines
        .iter()
        .filter(|l| !l.is_empty())
        .map(|l| &l[..indent(l)])
        .reduce(|a, b| {
            let same = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
            &a[..same]
        })
        .unwrap_or("");
    lines
        .iter()
        .map(|l| l.strip_prefix(margin).unwrap_or(l))
        .collect::<Vec<_>>()
        .join("\n")
}

/// メッセージの位置を (行, 桁, 幅) にする。ソースの範囲外なら None。桁と幅は文字の数。
///
/// 位置は送ったソース上の座標なので、#eval や括弧で包んだ分のずれ (line_off /
/// col_off) を引いてから、その行の長さに収める。col_off は `indent_code` が空白を
/// 足した行 (文字列の中で始まらない行) だけから引く。
pub fn span<S: AsRef<str>>(
    m: &Json,
    lines: &[S],
    line_off: i64,
    col_off: i64,
) -> Option<(usize, usize, usize)> {
    let null = Json::Null;
    let (pos, end) = (
        m.get("pos").unwrap_or(&null),
        m.get("endPos").unwrap_or(&null),
    );
    let ln = int(pos, "line").unwrap_or(0) - line_off;
    let src_line = lines.get(usize::try_from(ln - 1).ok()?)?.as_ref();
    let len = i64::try_from(src_line.chars().count()).ok()?;
    let src = lines
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join("\n");
    let col_off = match outside_strings(&src).get(usize::try_from(ln - 1).ok()?) {
        Some(true) => col_off,
        _ => 0,
    };
    let col = (int(pos, "column").unwrap_or(0) - col_off).clamp(0, len);
    let width = if int(end, "line").unwrap_or(0) - line_off == ln {
        (int(end, "column").unwrap_or(0) - col_off - col).max(1)
    } else {
        1
    };
    let width = width.min((len - col).max(1));
    Some((
        usize::try_from(ln).ok()?,
        usize::try_from(col).ok()?,
        usize::try_from(width).ok()?,
    ))
}

/// キャレットの前に置く空白。`line` の先頭から `col` 文字分の表示の幅にそろえる。
///
/// 全角文字は 2 桁分の空白にする。タブの幅は端末が決めるので、タブはタブのまま残す。
pub fn caret_pad(line: &str, col: usize) -> String {
    line.chars()
        .take(col)
        .map(|c| match c {
            '\t' => "\t".to_string(),
            c => " ".repeat(char_width(c)),
        })
        .collect()
}

/// `^` の数。`line` の `col` 文字目から `width` 文字分の表示の幅。1 より小さくしない。
pub fn caret_len(line: &str, col: usize, width: usize) -> usize {
    line.chars()
        .skip(col)
        .take(width)
        .map(char_width)
        .sum::<usize>()
        .max(1)
}

// ------------------------------------------------------------------ 定理検索

// loogle の応答を読む。エラーも検索結果も同じステータス 200 で返るので、`error`
// キーがあるかどうかだけで区別する。

/// loogle の応答を表示する形にする。1 件 2 行 (名前と型 / どの module か)。
pub fn loogle_text(got: &Json, keep: usize, width: usize, style: Style) -> String {
    let hits = list(got, "hits");
    match text(got, "error") {
        Some(err) if !err.is_empty() => {
            let suggest: Vec<&str> = list(got, "suggestions")
                .iter()
                .filter_map(Json::as_str)
                .collect();
            let head = style.red(&format!("loogle: {err}"));
            if suggest.is_empty() {
                head
            } else {
                let also = style.dim(&format!("もしかして: {}", suggest.join("  ")));
                format!("{head}\n{also}")
            }
        }
        _ if !hits.is_empty() => {
            let total = got
                .get("count")
                .and_then(Json::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(hits.len());
            loogle_hits(hits, total, keep, width, style)
        }
        _ => style.dim("見つからなかった"),
    }
}

/// 検索結果の先頭 keep 件を表示する形にする。
///
/// total は見つかった総数で、hits は loogle が 200 件までに切り詰めたもの。
/// 表示するのはさらにその先頭だけなので、全体の件数には total をそのまま使う。
pub fn loogle_hits(hits: &[Json], total: usize, keep: usize, width: usize, style: Style) -> String {
    let shown = &hits[..keep.min(hits.len())];
    let head = if shown.len() < total {
        format!("{total} 件 (先頭 {} 件)", shown.len())
    } else {
        format!("{total} 件")
    };
    std::iter::once(style.dim(&head))
        .chain(shown.iter().flat_map(|hit| hit_rows(hit, width, style)))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn hit_rows(hit: &Json, width: usize, style: Style) -> [String; 2] {
    let name = text(hit, "name").unwrap_or("?");
    let ty = text(hit, "type").unwrap_or("");
    let sig = format!("{name} :{ty}");
    let sig = sig.split_whitespace().collect::<Vec<_>>().join(" ");
    let module = text(hit, "module").unwrap_or("?");
    [clip(&sig, width), style.dim(&format!("  {module}"))]
}

// ------------------------------------------------------------------ #eval のエラー

/// #eval のエラー文から、評価できない理由。評価できないエラーでなければ None。
pub fn not_evaluable_reason(blob: &str) -> Option<&'static str> {
    if NONCOMPUTABLE.is_match(blob) {
        Some("noncomputable なので評価できない")
    } else if NO_REPR.is_match(blob) {
        Some("値を表示する Repr や ToString が無いので評価できない")
    } else if CANNOT_EVAL.is_match(blob) {
        Some("評価できない")
    } else {
        None
    }
}

/// #eval のエラー文が、do の外に `(← e)` を書いたというエラーか。
pub fn nested_action(blob: &str) -> bool {
    NESTED_ACTION.is_match(blob)
}

// ------------------------------------------------------------------ パス

/// 先頭の `~` だけをホームディレクトリにする (Python の `os.path.expanduser`)。
/// `~user` は展開しない。相対パスはそのまま残す。
pub fn expand_home(path: &str, home: &str) -> String {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{home}{rest}"),
        _ => path.to_string(),
    }
}

/// `~` を展開して絶対パスにする (Python の `os.path.abspath(os.path.expanduser(p))`)。
/// ファイルは見ないので、シンボリックリンクは解決しない。空なら None。
pub fn abspath(path: &str, home: &str, cwd: &str) -> Option<String> {
    let expanded = expand_home(path, home);
    match expanded.as_str() {
        "" => None,
        p if p.starts_with('/') => Some(normpath(p)),
        p => Some(normpath(&format!("{cwd}/{p}"))),
    }
}

/// `.` と `..` と重なった `/` を取り除く (Python の `posixpath.normpath`)。
/// 絶対パスだけを受け取る。先頭がちょうど `//` のときは、POSIX の決まりで残す。
fn normpath(path: &str) -> String {
    let root = if path.starts_with("//") && !path.starts_with("///") {
        "//"
    } else {
        "/"
    };
    let parts = path.split('/').fold(Vec::new(), |mut parts, part| {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
        parts
    });
    format!("{root}{}", parts.join("/"))
}
