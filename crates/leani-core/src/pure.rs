//! 判定と整形 (純粋)。
//!
//! 色付け、応答の読み取り、エンジンのバージョン、完結判定、表示の組み立てを扱う。
//! 入力だけで出力が決まるので、テストは入力と出力だけで書ける。
//!
//! repl の応答は `Json` のまま受け取る。キーが無いときや型が違うときは、Python 版の
//! `.get(key, default)` と同じく既定値として読む (`text`、`int`、`list`)。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;

use crate::queries::{
    BLOCK_OPEN, CANNOT_EVAL, ERR_POS, INCOMPLETE, NESTED_ACTION, NO_REPR, NONCOMPUTABLE,
};
use crate::types::{EngineError, Json, Kind, Proof, State};

// ------------------------------------------------------------------ 応答の読み取り

/// `v[key]` の文字列。無いか文字列でなければ None。
pub fn text<'a>(v: &'a Json, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Json::as_str)
}

/// `v[key]` の整数。無いか整数でなければ None。
pub fn int(v: &Json, key: &str) -> Option<i64> {
    v.get(key).and_then(Json::as_i64)
}

/// `v[key]` の配列。無いか配列でなければ空。
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
    /// 色を付けない。
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
/// JSON のエスケープは Lean の \n \t \\ \" \uXXXX と互換。
pub fn lean_str(s: &str) -> String {
    serde_json::to_string(s).expect("文字列は必ず JSON にできる")
}

/// 文字列の列を Lean の `#[...]` にする。
pub fn lean_strs<S: AsRef<str>>(xs: &[S]) -> String {
    let items: Vec<String> = xs.iter().map(|x| lean_str(x.as_ref())).collect();
    format!("#[{}]", items.join(", "))
}

// ------------------------------------------------------------------ 補完

pub const COMPLETE_DELIMS: &str = " \t\n(),[]{};\"";

/// text の末尾にある名前が始まる位置 (byte)。補完でカーソルの前の名前を切り出すため。
///
/// Lean の名前は `.` を含むので、`.` では区切らない。
pub fn name_start(text: &str) -> usize {
    // 区切り文字はすべて ASCII なので、見つけた位置の次の byte が名前の先頭になる
    text.rfind(|c| COMPLETE_DELIMS.contains(c))
        .map_or(0, |i| i + 1)
}

/// ヘルプに書いてある `:` コマンドの名前 (`:` は付けない)。補完の候補にする。
///
/// ヘルプから取り出すので、ヘルプに載せたコマンドだけが候補になる。`:{` と `:!` は
/// 名前ではないので含めない。
pub fn meta_names(help_text: &str) -> Vec<String> {
    static META: LazyLock<Regex> = LazyLock::new(|| Regex::new(r":(\w+)").expect("正規表現"));
    // Python 版の `(?<![\w:])` の代わりに、`:` の直前の文字を確かめる
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

/// 先頭のヘッダ領域にある import を、モジュール名と行番号で返す。
///
/// ヘッダ領域は、空行、コメント、`import` が続く範囲。行単位で `import` を
/// 探すと、doc コメントに書いた `import Foo` を本物の import と取り違える。
/// それを :save のヘッダに書き出すと、repl はヘッダ全体を無視して起動するので、
/// 書き出したファイルは :l でも lean でもエラーになる。import 行を取り除く処理
/// でも同じで、コメントの行を消してコメントをおかしくする。
pub fn scan_header(text: &str) -> (Vec<String>, BTreeSet<usize>) {
    let mut mods = Vec::new();
    let mut at = BTreeSet::new();
    let mut depth = 0;

    for (n, line) in text.lines().enumerate() {
        let one = line.trim();
        if depth > 0 {
            depth = comment_depth(line, depth);
        } else if one.is_empty() || one.starts_with("--") {
            continue;
        } else if one.starts_with("/-") {
            depth = comment_depth(line, 0);
        } else if let Some(module) = import_of(one) {
            mods.push(module.to_string());
            at.insert(n);
        } else {
            break; // ヘッダ領域はここで終わり。以降の import は Lean も認めない
        }
    }

    (mods, at)
}

/// `import X` の行なら X。前後の空白は取り除いてある行を受け取る。
fn import_of(one: &str) -> Option<&str> {
    match one.split_once(char::is_whitespace) {
        Some(("import", rest)) if !one.contains("/-") => Some(rest.trim()),
        _ => None,
    }
}

/// `import X` の X を並べる。:save のヘッダに書き戻すため。
pub fn import_lines(text: &str) -> Vec<String> {
    scan_header(text).0
}

/// import 行を取り除く。既存の環境の上で実行するときに使う。
///
/// import はファイルの先頭にしか書けない。
pub fn strip_imports(text: &str) -> String {
    let at = scan_header(text).1;
    text.lines()
        .enumerate()
        .filter(|(n, _)| !at.contains(n))
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n")
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
pub fn hang(script: &str, pad: &str) -> String {
    script
        .split('\n')
        .enumerate()
        .map(|(n, one)| {
            if n > 0 && !one.trim().is_empty() {
                format!("{pad}{one}")
            } else {
                one.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
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

/// 自分でビルドしたエンジンの toolchain が、使うバージョンと違うときの警告の文。
///
/// leani が用意したエンジン (own が false) は使うバージョンでビルドしてあるので、
/// バージョンは必ず一致する。自分でビルドしたエンジンだけ、バージョンが違って
/// いたら警告する (leani がビルドし直すことはしない)。engine_tc が空なら、
/// エンジンの lean-toolchain が読めなかったので比べない。
pub fn toolchain_warning(own: bool, engine_tc: &str, tc: &str, engine_dir: &str) -> Option<String> {
    if own && !engine_tc.is_empty() && engine_tc != tc {
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

/// パーサの応答の `probe[cat]` が `{"ok": true}` か。
fn parsed(probe: &Json, cat: &str) -> bool {
    probe.get(cat).and_then(|p| p.get("ok")) == Some(&Json::Bool(true))
}

/// パーサの応答の `probe[cat]["err"]`。無ければ空文字列。
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

/// 証明モードでは tacticSeq として読めるかだけを見る。
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

/// 最後のタクティクを取り消した Proof。取り消せるタクティクが無ければ None。
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
    let mut stack = Vec::new();
    for ch in src.chars() {
        if let Some(&(_, close)) = PAIRS.iter().find(|(open, _)| *open == ch) {
            stack.push(close);
        } else if PAIRS.iter().any(|(_, close)| *close == ch) && stack.pop() != Some(ch) {
            return false;
        }
    }
    stack.is_empty()
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
/// 位置は送ったソース上の座標なので、#eval でラップした分のずれ (line_off /
/// col_off) を引いてから、その行の長さに収める。
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

/// 検索結果 1 件を 2 行 (名前と型 / どの module か) にする。
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

/// `~` を展開して絶対パスにする (Python の `os.path.abspath(os.path.expanduser(p))`)。
/// ファイルは見ないので、シンボリックリンクは解決しない。空なら None。
pub fn abspath(path: &str, home: &str, cwd: &str) -> Option<String> {
    let expanded = match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{home}{rest}"),
        _ => path.to_string(),
    };
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
