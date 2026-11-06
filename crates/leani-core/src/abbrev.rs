//! 略記の表と展開 (純粋)。
//!
//! `\to` を `→` にする表。VS Code の Lean 拡張と同じ表記と同じ値を使う。
//!
//! 表は、VS Code の Lean 拡張にある 1857 件のうち 1829 件を使う。値も元の表のままなので、
//! `\d` が δ ではなく ↓ になるような意外な対応もそのままにしてある。leani とエディタで
//! 対応が違うと、ユーザーはどちらで入力しているかを意識しながら入力することになる。
//!   https://github.com/leanprover/vscode-lean4/blob/master/lean4-unicode-input/src/abbreviations.json
//!
//! 除いたのは 2 種類。`{}` → `{$CURSOR}` のように変換後のカーソル位置を持つ 26 件は、
//! 置き換えたあとにカーソルを戻す仕組みが必要になる。`\` と `+ ` の 2 件は space か Tab で
//! 確定する方式では入力できない (`\` は、直前の `\` より後ろをキーとして表を検索するので
//! キーが空になる。`+ ` は、キーの中の空白で先に確定してしまう)。
//!
//! 表はキーの順に並べてある。293 件が他のキーの接頭辞なので、意味で並べても探しやすくは
//! ならない。表はこの crate の `abbrev.json` に置き、ここに埋め込む。Python 版は、この
//! ファイルへの symlink の `src/leani/abbrev.json` を読む。
//!
//! キーはすべて ASCII なので、キーの文字数と byte 数は同じ。

use std::collections::BTreeMap;
use std::ops::Bound;
use std::sync::LazyLock;

use crate::width::{WIDE, ZERO};

/// 略記の表。キーの順に並ぶ (`:abbrev` もこの順に表示する)。
pub static ABBREV: LazyLock<BTreeMap<String, String>> =
    LazyLock::new(|| serde_json::from_str(include_str!("../abbrev.json")).expect("abbrev.json"));

/// カーソルの手前にある `\name` を記号に置き換える。(記号, 消す文字数) を返す。
/// space か Tab を押したときに呼ぶ。キーを押すたびに呼ばない理由は DECISIONS.md にある。
///
/// 表に無ければ None を返す。`\` のあとを空白まで取り出して、そのまま表を検索
/// するので、`\to)` のように記号が続いた形は変換しない (先に Tab で変換する)。
pub fn expand_abbrev(head: &str) -> Option<(&'static str, usize)> {
    let cut = head.rfind('\\')?;
    let name = &head[cut + 1..];
    ABBREV.get(name).map(|sym| (sym.as_str(), name.len() + 1))
}

/// カーソルの手前が `\name` の途中なら、name を返す。Tab で略記を補完するときに使う。
///
/// name は直前の `\` より後ろで、どれかのキーの接頭辞になっている。`\` の直後なら
/// 空文字列を返す。略記の途中でなければ None を返す。`"a\n".leng` の `\n".leng` の
/// ような文字列のエスケープを略記と読むと、名前を補完しなくなる。
pub fn abbrev_head(head: &str) -> Option<&str> {
    let cut = head.rfind('\\')?;
    let name = &head[cut + 1..];
    let key = ABBREV
        .range::<str, _>((Bound::Included(name), Bound::Unbounded))
        .next();
    if key.is_some_and(|(k, _)| k.starts_with(name)) {
        Some(name)
    } else {
        None
    }
}

/// `\name` を置き換える記号の候補を返す。端末と Jupyter の Tab 補完で使う。
///
/// name と一致するキーの記号を先頭に置き、name で始まるほかのキーの記号をキーの
/// 順に続ける。同じ記号は 1 回だけ返す。name が空なら、候補が多すぎるので返さない。
pub fn abbrev_candidates(name: &str) -> Vec<&'static str> {
    if name.is_empty() {
        Vec::new()
    } else {
        let exact = ABBREV.get(name).into_iter();
        let longer = ABBREV
            .range::<str, _>((Bound::Included(name), Bound::Unbounded))
            .take_while(|(k, _)| k.starts_with(name))
            .filter(|(k, _)| *k != name)
            .map(|(_, s)| s);
        exact.chain(longer).fold(Vec::new(), |mut seen, s| {
            if !seen.contains(&s.as_str()) {
                seen.push(s.as_str());
            }
            seen
        })
    }
}

/// `:abbrev` で表示する (キー, 記号) を、キーの順に返す。
///
/// arg がすべて ASCII なら、arg で始まるキーを返す (先頭の `\` は無くてもよい)。
/// それ以外なら記号とみなして、その記号を入力できるキーを返す。端末では
/// `:abbrev \le` と入力して space を押すと `:abbrev ≤` になるので、どちらの形でも
/// 調べられるようにする。
pub fn abbrev_lookup(arg: &str) -> Vec<(&'static str, &'static str)> {
    let pairs = ABBREV.iter().map(|(k, s)| (k.as_str(), s.as_str()));
    if arg.is_ascii() {
        let key = arg.strip_prefix('\\').unwrap_or(arg);
        pairs.filter(|(k, _)| k.starts_with(key)).collect()
    } else {
        pairs.filter(|(_, s)| *s == arg).collect()
    }
}

/// 端末で表示したときの幅。全角は 2、結合文字は 0。全角か半角かが決まっていない
/// 文字は 1 とする。
pub fn cell_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

pub fn char_width(c: char) -> usize {
    let within = |table: &[(char, char)]| {
        table
            .binary_search_by(|&(lo, hi)| {
                if hi < c {
                    std::cmp::Ordering::Less
                } else if lo > c {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok()
    };
    if within(ZERO) {
        0
    } else if within(WIDE) {
        2
    } else {
        1
    }
}

/// 列と列のあいだの空白。
const GAP: usize = 3;

/// (キー, 記号) を `man ascii` のように複数の列に並べる。縦に並べ、列の下まで行ったら
/// 右の列に続ける。列の数は、幅に収まる範囲でいちばん多くする。
pub fn abbrev_table(pairs: &[(&str, &str)], width: usize) -> String {
    let cells: Vec<(String, &str)> = pairs.iter().map(|(k, s)| (format!("\\{k}"), *s)).collect();
    let layout = (1..=cells.len()).rev().find_map(|n| {
        let rows = cells.len().div_ceil(n);
        let cols: Vec<&[(String, &str)]> = cells.chunks(rows).collect();
        if fits(&cols, width) || n == 1 {
            Some((rows, cols))
        } else {
            None
        }
    });
    match layout {
        Some((rows, cols)) => (0..rows)
            .map(|r| table_row(&cols, r))
            .collect::<Vec<_>>()
            .join("\n"),
        None => String::new(),
    }
}

/// 1 列のキーの幅と記号の幅。
fn column_widths(col: &[(String, &str)]) -> (usize, usize) {
    let key = col
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);
    let sym = col.iter().map(|(_, s)| cell_width(s)).max().unwrap_or(0);
    (key, sym)
}

fn fits(cols: &[&[(String, &str)]], width: usize) -> bool {
    let used: usize = cols
        .iter()
        .map(|col| {
            let (k, s) = column_widths(col);
            k + 1 + s
        })
        .sum::<usize>()
        + GAP * (cols.len() - 1);
    used <= width
}

/// r 行目。列ごとにキーと記号の幅をそろえる。
fn table_row(cols: &[&[(String, &str)]], r: usize) -> String {
    let parts: Vec<String> = cols
        .iter()
        .map(|col| match col.get(r) {
            Some((key, sym)) => {
                let (kw, sw) = column_widths(col);
                let pad = " ".repeat(sw - cell_width(sym));
                format!("{key:<kw$} {sym}{pad}")
            }
            None => String::new(),
        })
        .collect();
    parts.join(&" ".repeat(GAP)).trim_end().to_string()
}
