//! 略記の表と展開 (純粋)。
//!
//! `\to` を `→` にする表。VS Code の Lean 拡張と同じ表記と同じ値を使う。表は Python 版と
//! 共有するので `src/leani/abbrev.json` に置き、ここに埋め込む。表の出どころと、除いた
//! 略記は `src/leani/abbrev.py` の冒頭に書いてある。
//!
//! キーはすべて ASCII なので、キーの文字数と byte 数は同じ。

use std::collections::BTreeMap;
use std::ops::Bound;
use std::sync::LazyLock;

use crate::width::{WIDE, ZERO};

/// 略記の表。キーの順に並ぶ (`:abbrev` もこの順に表示する)。
pub static ABBREV: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../src/leani/abbrev.json")).expect("abbrev.json")
});

/// カーソルの手前にある `\name` を記号に置き換える。(記号, 消す文字数) を返す。
///
/// 確定は space か Tab で行い、キー入力ごとには変換しない。`\a` `\all` `\alpha`
/// のようにほかのキーの接頭辞になっているキーが 293 件あり、入力するたびに確定
/// すると `\alpha` を入力できなくなる。space か Tab まで待てば、どのキーを入力した
/// のかが一意に決まる。
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
/// name は直前の `\` より後ろで、空白を含まない。`\` の直後なら空文字列を返す。
/// 略記の途中でなければ None を返す。
pub fn abbrev_head(head: &str) -> Option<&str> {
    let cut = head.rfind('\\')?;
    let name = &head[cut + 1..];
    if name.chars().any(char::is_whitespace) {
        None
    } else {
        Some(name)
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

/// 端末で表示したときの 1 文字の幅。
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
