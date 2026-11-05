//! テストとユーザーストーリーから `SPEC.md` を生成する。手で書いた一覧は必ずコードより
//! 古くなるので、`SPEC.md` に手で書き足す場所は作らない。
//!
//! ```text
//! stories.toml                誰の何を助けるか。コードからは分からないので、ここが情報源
//! crates/leani/tests/*.rs     それを確かめているテスト。テスト名がそのまま仕様の文になる
//! tests/test_*.py             Jupyter のカーネルのテスト (Python 版)
//! ```
//!
//! ```text
//! cargo run -p spec              # SPEC.md を書き直す
//! cargo run -p spec -- --check   # 生成結果と違えば 1 を返す
//! ```
//!
//! 各テストは `#[story(...)]` (カーネルのテストでは `@story(...)`) で、対象にするストーリーを
//! 指定する。対応は多対多でよい。テストの分け方はテストの都合 (失敗する理由が 1 つになるように、
//! なるべく速い層でテストするように) で決め、ストーリーは、テストが足りているかを数える表として
//! 使う。テストが 0 件のストーリーには「確かめているテストが無い。」と表示し、`unbuilt` にある
//! ストーリーには「まだ作っていない。」と表示する。
//!
//! Rust では、`#[test]` か `#[proptest]` の付いた関数をテストと見なし、テストを含む `mod` を
//! describe と見なす。
//!
//! Python のテストは行単位で読む。`def describe_` の行をインデントで入れ子にし、その中で
//! `@story(...)` の行の次にある `def` をテストと見なす。describe の名前からは `describe_`
//! を外す。Python の構文を解析する crate は使わない。カーネルのテストは数が少なく、
//! 書き方も決まっているので、行単位で足りる。
//!
//! テスト名を文に戻す規則は 5 つだけ。
//!
//! ```text
//! __                     ハイフン   Ctrl__C     -> Ctrl-C
//! 語の先頭の colon_      :          colon_save  -> :save (関数名に : を書けないため)
//! 語の先頭の dash_       -          dash_e      -> -e / dash_dash_setup -> --setup
//! 片側が ASCII の _      空白       do_by       -> do by / IO_の式 -> IO の式
//! それ以外の _           詰める     行を_待つ   -> 行を待つ
//! ```
//!
//! Python は識別子を NFKC で正規化して読むので、ここでも名前を NFKC にしてから戻す。

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use syn::punctuated::Punctuated;
use syn::{Attribute, Item, LitStr, Token};
use unicode_normalization::UnicodeNormalization;

/// 生成するファイル。
pub const OUT: &str = "SPEC.md";

const HEAD: &str =
    "<!-- crates/spec が crates/leani/tests/ と tests/ から生成する。手で編集しない。 -->

# leani の性質一覧

leani が誰の何を助けるか (`stories.toml`) に、それを確かめているテストの
名前を並べたもの。テスト名はすべて `cargo nextest run` (層 kernel は `uv run pytest`)
で実際に確かめられている。
項目が増えたり減ったりするのは、ストーリーかテストを追加または削除したときだけ。

    cargo run -p spec

";

/// リポジトリの直下。cwd によらずに決まるように、この crate の場所から求める。
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

// ---------------------------------------------------------------- ストーリー

#[derive(Deserialize)]
pub struct Group {
    pub id: String,
    pub title: String,
}

#[derive(Deserialize)]
pub struct Story {
    pub id: String,
    pub text: String,
}

/// `stories.toml` の中身。書いた順に `SPEC.md` に並べるので、`[[group]]` と `[[story]]` の
/// 配列で書く。各ストーリーは、id の先頭の文字と同じ id の group の下に並ぶ。
#[derive(Deserialize)]
pub struct Stories {
    pub unbuilt: Vec<String>,
    pub group: Vec<Group>,
    pub story: Vec<Story>,
}

impl Stories {
    fn has(&self, sid: &str) -> bool {
        self.story.iter().any(|s| s.id == sid)
    }
}

pub fn stories(root: &Path) -> Result<Stories, String> {
    let path = root.join("stories.toml");
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

// ---------------------------------------------------------------- 名前を戻す

/// ASCII の英数字か `:` か `-` の 1 文字か。`:` と `-` は `:save` や `-e` のような
/// コマンド名とオプションの先頭なので、英数字と同じく前の `_` を空白にする。名前の端
/// では `None` が渡されるので、その場合は false。
fn ascii_word(ch: Option<char>) -> bool {
    ch.is_some_and(|c| c.is_ascii_alphanumeric() || c == ':' || c == '-')
}

/// `_` を単語の区切りとして扱う。前後のどちらかが `ascii_word` なら空白にする。
fn spaced(part: &str) -> String {
    let chars: Vec<char> = part.chars().collect();
    chars
        .iter()
        .enumerate()
        .fold(String::new(), |out, (i, &ch)| match ch {
            '_' if !out.is_empty()
                && (ascii_word(out.chars().last()) || ascii_word(chars.get(i + 1).copied())) =>
            {
                out + " "
            }
            '_' => out,
            c => {
                let mut out = out;
                out.push(c);
                out
            }
        })
}

/// 語の先頭 (文字列の先頭か `_` の直後) にある `from` を `to` に置き換える。
fn at_word_start(s: &str, from: &str, to: &str) -> String {
    let mut out = String::new();
    let mut prev: Option<char> = None;
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if rest.starts_with(from) && prev.is_none_or(|p| p == '_') {
            out.push_str(to);
            prev = from.chars().last();
            rest = &rest[from.len()..];
        } else {
            out.push(c);
            prev = Some(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// 語の先頭の `colon_` を `:` に、`dash_` を `-` に戻す。関数名に書けない記号。
fn symbols(part: &str) -> String {
    at_word_start(&at_word_start(part, "colon_", ":"), "dash_", "-")
}

/// テストの関数名か describe の `mod` の名前を、仕様の 1 文に戻す。
pub fn label(name: &str) -> String {
    let name: String = name.nfkc().collect();
    name.split("__")
        .map(|p| spaced(&symbols(p)))
        .collect::<Vec<_>>()
        .join("-")
}

// ------------------------------------------------------------ テストを集める

/// テスト 1 件。`what` は関数名を文に戻したもの、`place` は「層 / describe」。
pub struct Test {
    pub what: String,
    pub place: String,
    pub stories: Vec<String>,
}

/// `#[story(...)]` に書かれたストーリー番号。付いていなければ空。
fn marked(attrs: &[Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter(|a| a.path().segments.last().is_some_and(|s| s.ident == "story"))
        .flat_map(|a| {
            a.parse_args_with(Punctuated::<LitStr, Token![,]>::parse_terminated)
                .map(|ids| ids.iter().map(LitStr::value).collect::<Vec<_>>())
                .unwrap_or_default()
        })
        .collect()
}

/// `#[test]` か `#[proptest]` が付いているか。テストはこの 2 つの属性で見分ける。
fn is_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path()
            .segments
            .last()
            .is_some_and(|s| s.ident == "test" || s.ident == "proptest")
    })
}

/// `#[ignore]` か、`cfg(test)` 以外の `#[cfg]` が付いているか。どちらのテストも、
/// `cargo nextest run` で実行されないことがある。
fn skipped(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("ignore")
            || (a.path().is_ident("cfg")
                && a.meta
                    .require_list()
                    .map_or(true, |l| l.tokens.to_string() != "test"))
    })
}

/// `mod x;` の中身。`dir` にある `x.rs` か `x/mod.rs` を読む。
fn mod_file(dir: &Path, name: &str) -> Result<syn::File, String> {
    let path = [
        dir.join(format!("{name}.rs")),
        dir.join(name).join("mod.rs"),
    ]
    .into_iter()
    .find(|p| p.is_file())
    .ok_or_else(|| format!("mod {name}; のファイルが無い: {}", dir.display()))?;
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    syn::parse_file(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// `mod` を再帰的にたどってテストを集める。テストを含む `mod` が describe になる。
/// `mod` は入れ子にでき、describe の名前は外側の `mod` から ` / ` でつなぐ。
/// `dir` は、`mod x;` と書いたときに `x` のファイルを探すディレクトリ。
///
/// 次の書き方はエラーにする。どれも、`SPEC.md` と実行されるテストがずれるのに気づけない。
///
/// ```text
/// #[story] が付いているのにテストの属性が無い関数   実行されないのに載る
/// #[ignore] か #[cfg] が付いたテストと mod         実行されないことがあるのに載る
/// 直下のマクロ (proptest! など)                   中のテストを読めないので載らない
/// #[path] の付いた mod                            ファイルを探さないので載らない
/// ```
fn descend(
    items: &[Item],
    layer: &str,
    place: Option<&str>,
    dir: &Path,
) -> Result<Vec<Test>, String> {
    let here = place.unwrap_or(layer);
    items
        .iter()
        .map(|item| match item {
            Item::Mod(m) if m.attrs.iter().any(|a| a.path().is_ident("path")) => Err(format!(
                "#[path] の付いた mod は読まない: {layer} / {here} / {}",
                m.ident
            )),
            Item::Mod(m) => {
                let ident = m.ident.to_string();
                let name = label(&ident);
                let inside = match place {
                    Some(outer) => format!("{outer} / {name}"),
                    None => name,
                };
                let sub = dir.join(&ident);
                let tests = match &m.content {
                    Some((_, inner)) => descend(inner, layer, Some(&inside), &sub)?,
                    None => descend(&mod_file(dir, &ident)?.items, layer, Some(&inside), &sub)?,
                };
                match (skipped(&m.attrs), tests.is_empty()) {
                    (true, false) => {
                        Err(format!("実行されないことがあるテスト: {layer} / {inside}"))
                    }
                    _ => Ok(tests),
                }
            }
            Item::Macro(m) if m.ident.is_none() => Err(format!(
                "マクロの中のテストは読めない: {layer} / {here} / {}!",
                m.mac
                    .path
                    .segments
                    .last()
                    .map_or(String::new(), |s| s.ident.to_string())
            )),
            Item::Fn(f) if is_test(&f.attrs) && skipped(&f.attrs) => Err(format!(
                "実行されないことがあるテスト: {layer} / {here} / {}",
                f.sig.ident
            )),
            Item::Fn(f) if is_test(&f.attrs) => Ok(vec![Test {
                what: label(&f.sig.ident.to_string()),
                place: format!("{layer} / {here}"),
                stories: marked(&f.attrs),
            }]),
            Item::Fn(f) if !marked(&f.attrs).is_empty() => Err(format!(
                "テストでない関数に #[story] が付いている: {layer} / {here} / {}",
                f.sig.ident
            )),
            _ => Ok(Vec::new()),
        })
        .collect::<Result<Vec<_>, String>>()
        .map(|v| v.into_iter().flatten().collect())
}

/// Python の 1 行を左から読んでいるときの状態。
#[derive(Clone, Copy)]
enum Lex {
    Code,
    /// `'` か `"` の文字列。行の中で閉じる。
    Short(char),
    /// 三重引用符の文字列。行をまたいで続く。
    Long(&'static str),
}

/// 1 行を左から読み、行の終わりで開いたままの三重引用符を返す。`open` は、行の始まりで
/// 開いている三重引用符。引用符の中の `#` と三重引用符、`#` のコメントの中の引用符は
/// 文字列の始まりと終わりにしない。三重引用符を閉じた行の残りも読む。
fn string_after(line: &str, open: Option<&'static str>) -> Option<&'static str> {
    let mut lex = open.map_or(Lex::Code, Lex::Long);
    let mut rest = line;
    while let Some(c) = rest.chars().next() {
        let triple = if c == '"' { "\"\"\"" } else { "'''" };
        let (next, used) = match (lex, c) {
            (Lex::Code, '#') => return None,
            (Lex::Code, '"' | '\'') if rest.starts_with(triple) => (Lex::Long(triple), 3),
            (Lex::Code, '"' | '\'') => (Lex::Short(c), 1),
            (Lex::Short(_) | Lex::Long(_), '\\') => {
                (lex, 1 + rest[1..].chars().next().map_or(0, char::len_utf8))
            }
            (Lex::Short(q), c) if c == q => (Lex::Code, 1),
            (Lex::Long(q), _) if rest.starts_with(q) => (Lex::Code, 3),
            _ => (lex, c.len_utf8()),
        };
        lex = next;
        rest = &rest[used..];
    }
    match lex {
        Lex::Long(q) => Some(q),
        Lex::Code | Lex::Short(_) => None,
    }
}

/// Python のテストファイルを行単位で読む。describe はインデントで入れ子にする。
///
/// 三重引用符の中の行は読まない。セルの中身として書いた `def fib` を関数と見なすと、
/// インデントが 0 なので、開いている describe を閉じてしまう。
///
/// describe の中で `@story` を付けた関数をテストと見なす。pytest-describe は describe の
/// 中の `_` で始まらない関数をすべてテストとして集めるので、名前に接頭辞は要らない。
/// describe の外や `class` の中で `@story` を付けた関数はエラーにする。pytest-describe は
/// これを集めないので、実行されないテストが `SPEC.md` に載る。反対に、`@story` の無い
/// テストは `tests/stories.py` が実行する前に失敗させる。
fn python_tests(text: &str, layer: &str) -> Result<Vec<Test>, String> {
    struct Scan {
        /// 開いているブロックの (インデント, describe の名前)。`class` は名前が None。
        describes: Vec<(usize, Option<String>)>,
        /// 次の `def` に付く `@story` の番号。
        pending: Vec<String>,
        /// 開いている三重引用符。閉じるまで行を読まない。
        string: Option<&'static str>,
        out: Vec<Test>,
    }
    let start = Scan {
        describes: Vec::new(),
        pending: Vec::new(),
        string: None,
        out: Vec::new(),
    };
    let done = text.lines().try_fold(start, |mut scan, line| {
        let inside = scan.string;
        scan.string = string_after(line, inside);
        if inside.is_some() {
            return Ok(scan);
        }
        let body = line.trim_start();
        let indent = line.len() - body.len();
        let name = body
            .strip_prefix("async ")
            .unwrap_or(body)
            .strip_prefix("def ")
            .and_then(|rest| rest.split('(').next());
        if body.starts_with("@story(") {
            scan.pending
                .extend(body.split('"').skip(1).step_by(2).map(str::to_string));
            return Ok(scan);
        }
        if body.starts_with("class ") {
            scan.describes.retain(|(at, _)| *at < indent);
            scan.describes.push((indent, None));
            return Ok(scan);
        }
        let Some(name) = name else {
            return Ok(scan);
        };
        scan.describes.retain(|(at, _)| *at < indent);
        let stories = std::mem::take(&mut scan.pending);
        if let Some(d) = name.strip_prefix("describe_") {
            if !stories.is_empty() {
                return Err(format!("describe に @story が付いている: {layer} / {name}"));
            }
            scan.describes.push((indent, Some(label(d))));
            return Ok(scan);
        }
        if stories.is_empty() {
            return Ok(scan);
        }
        let names: Option<Vec<&str>> = scan.describes.iter().map(|(_, d)| d.as_deref()).collect();
        let Some(place) = names.filter(|n| !n.is_empty()).map(|n| n.join(" / ")) else {
            return Err(format!(
                "describe の外のテストは pytest-describe が実行しない: {layer} / {name}"
            ));
        };
        scan.out.push(Test {
            what: label(name),
            place: format!("{layer} / {place}"),
            stories,
        });
        Ok(scan)
    })?;
    Ok(done.out)
}

/// ディレクトリにある、名前が `prefix` で始まり拡張子が `ext` のファイルを、
/// (層の名前, パス) にして返す。層の名前はファイル名から `prefix` と拡張子を外したもの。
fn layers(dir: &Path, prefix: &str, ext: &str) -> Result<Vec<(String, PathBuf)>, String> {
    Ok(fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == ext))
        .filter_map(|p| {
            let stem = p.file_stem()?.to_string_lossy().into_owned();
            Some((stem.strip_prefix(prefix)?.to_string(), p))
        })
        .collect())
}

/// テストを、層の名前の順、ファイル内で出てくる順に返す。
pub fn collect(root: &Path) -> Result<Vec<Test>, String> {
    let mut files = layers(&root.join("crates/leani/tests"), "", "rs")?;
    files.extend(layers(&root.join("tests"), "test_", "py")?);
    files.sort();
    files
        .iter()
        .map(|(layer, path)| {
            let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            if path.extension().is_some_and(|x| x == "py") {
                return python_tests(&text, layer);
            }
            let file = syn::parse_file(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            let dir = path.parent().unwrap_or(Path::new("."));
            descend(&file.items, layer, None, dir)
        })
        .collect::<Result<Vec<_>, String>>()
        .map(|v| v.into_iter().flatten().collect())
}

/// 2 回以上出てくる値。出てきた順に 1 回ずつ返す。
fn repeated<'a>(items: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let all: Vec<&str> = items.collect();
    all.iter()
        .enumerate()
        .filter(|&(i, x)| all[..i].iter().filter(|y| *y == x).count() == 1)
        .map(|(_, x)| *x)
        .collect()
}

/// `stories.toml` の中だけで分かる誤り。ID の重複と、どの group にも入らないストーリー。
/// group に入らないストーリーは `SPEC.md` に出てこないので、テストがあっても気づけない。
fn story_complaints(stories: &Stories) -> Vec<String> {
    let dup_story = repeated(stories.story.iter().map(|s| s.id.as_str()))
        .into_iter()
        .map(|id| format!("story の番号が重複している: {id}"));
    let dup_group = repeated(stories.group.iter().map(|g| g.id.as_str()))
        .into_iter()
        .map(|id| format!("group の番号が重複している: {id}"));
    let orphan = stories
        .story
        .iter()
        .filter(|s| !stories.group.iter().any(|g| s.id.starts_with(&g.id)))
        .map(|s| format!("どの group にも入らない: {}", s.id));
    dup_story.chain(dup_group).chain(orphan).collect()
}

/// 生成を止める理由。`#[story]` の付け忘れ、ストーリー番号の書き間違い、
/// `stories.toml` の誤り、同じ場所の同じ名前のテスト。
/// 同じ名前の Python の関数は後の定義が前の定義を隠すので、前のテストは実行されない。
pub fn complaints(stories: &Stories, tests: &[Test]) -> Vec<String> {
    let built = stories
        .unbuilt
        .iter()
        .filter(|sid| tests.iter().any(|t| t.stories.contains(sid)))
        .map(|sid| format!("unbuilt にあるのにテストがある: {sid}"));
    let places: Vec<String> = tests
        .iter()
        .map(|t| format!("{} / {}", t.place, t.what))
        .collect();
    let same = repeated(places.iter().map(String::as_str))
        .into_iter()
        .map(|p| format!("同じ名前のテストがある: {p}"));
    story_complaints(stories)
        .into_iter()
        .chain(built)
        .chain(same)
        .chain(test_complaints(stories, tests))
        .collect()
}

/// テストごとの誤りと、`unbuilt` の番号の書き間違い。
fn test_complaints(stories: &Stories, tests: &[Test]) -> Vec<String> {
    let per_test = tests.iter().flat_map(|t| {
        let missing = t
            .stories
            .is_empty()
            .then(|| format!("story が無い: {} / {}", t.place, t.what));
        let unknown = t
            .stories
            .iter()
            .filter(|sid| !stories.has(sid))
            .map(|sid| format!("stories.toml に無い番号: {sid} ({})", t.what));
        missing.into_iter().chain(unknown)
    });
    let unbuilt = stories
        .unbuilt
        .iter()
        .filter(|sid| !stories.has(sid))
        .map(|sid| format!("unbuilt にあるのに story に無い: {sid}"));
    per_test.chain(unbuilt).collect()
}

// ------------------------------------------------------------------- 組み立て

fn section(stories: &Stories, tests: &[Test], story: &Story) -> String {
    let lines: Vec<String> = tests
        .iter()
        .filter(|t| t.stories.contains(&story.id))
        .map(|t| format!("- {} ({})", t.what, t.place))
        .collect();
    let body = match (lines.is_empty(), stories.unbuilt.contains(&story.id)) {
        (false, _) => lines.join("\n") + "\n",
        (true, true) => "まだ作っていない。\n".to_string(),
        (true, false) => "**確かめているテストが無い。**\n".to_string(),
    };
    format!("### {} {}\n\n{body}", story.id, story.text)
}

pub fn render(stories: &Stories, tests: &[Test]) -> Result<String, Vec<String>> {
    let bad = complaints(stories, tests);
    if !bad.is_empty() {
        return Err(bad);
    }
    let head = format!(
        "{HEAD}ストーリー {} 件、テスト {} 件。括弧の中はテストの場所\n(層 / describe)。\n",
        stories.story.len(),
        tests.len()
    );
    let groups = stories.group.iter().filter_map(|g| {
        let ids: Vec<&Story> = stories
            .story
            .iter()
            .filter(|s| s.id.starts_with(&g.id))
            .collect();
        if ids.is_empty() {
            None
        } else {
            Some(
                std::iter::once(format!("## {}. {}\n", g.id, g.title))
                    .chain(ids.iter().map(|s| section(stories, tests, s)))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
    });
    Ok(std::iter::once(head)
        .chain(groups)
        .collect::<Vec<_>>()
        .join("\n"))
}

/// `stories.toml` とテストから生成した中身。
pub fn generate(root: &Path) -> Result<String, String> {
    let stories = stories(root)?;
    let tests = collect(root)?;
    render(&stories, &tests).map_err(|bad| bad.join("\n"))
}

/// 生成結果が `OUT` と同じか確かめる。違えば理由を返す。
pub fn check(root: &Path) -> Result<(), String> {
    let want = generate(root)?;
    match fs::read_to_string(root.join(OUT)) {
        Ok(have) if have == want => Ok(()),
        _ => Err(format!("{OUT} が古い: cargo run -p spec")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 名前を文に戻す() {
        for (name, want) in [
            ("Ctrl__C_で止める", "Ctrl-C で止める"),
            ("colon_save_で書き出す", ":save で書き出す"),
            ("dash_e_で選ぶ", "-e で選ぶ"),
            ("dash_dash_setup_で用意する", "--setup で用意する"),
            ("do_by", "do by"),
            ("IO_の式", "IO の式"),
            ("行を_待つ", "行を待つ"),
            (
                "SPEC_md_がテストとそろっている",
                "SPEC md がテストとそろっている",
            ),
        ] {
            assert_eq!(label(name), want, "{name}");
        }
    }

    #[test]
    fn テストの属性が付いた関数だけを集める() {
        let file = syn::parse_file(
            r#"
            mod 式の評価 {
                fn helper() {}
                #[story("A1")]
                #[test]
                fn 単独の式は評価される() {}
                #[story("E2")]
                #[proptest]
                fn 連結しても閉じている(a: String) {}
            }
            mod common {
                pub fn drive() {}
            }
            "#,
        )
        .unwrap();
        let got: Vec<(String, String, Vec<String>)> =
            descend(&file.items, "parsing", None, Path::new("."))
                .unwrap()
                .into_iter()
                .map(|t| (t.place, t.what, t.stories))
                .collect();
        assert_eq!(
            got,
            [
                (
                    "parsing / 式の評価".to_string(),
                    "単独の式は評価される".to_string(),
                    vec!["A1".to_string()],
                ),
                (
                    "parsing / 式の評価".to_string(),
                    "連結しても閉じている".to_string(),
                    vec!["E2".to_string()],
                ),
            ]
        );
    }

    #[test]
    fn describe_の中で_story_を付けた関数をテストとして読む() {
        let text = r#"
def describe_セルの実行():

    @story("J1", "E3")
    @pytest.mark.usefixtures("place")
    def colon_q_は何もしない(kernel):
        pass

    @pytest.fixture
    def helper():
        pass

    def 補助():
        pass

def describe_カーネルの登録():

    @story("J1")
    def 複数行の(
        tmp_path,
    ):
        pass
"#;
        let got: Vec<(String, String, Vec<String>)> = python_tests(text, "kernel")
            .unwrap()
            .into_iter()
            .map(|t| (t.place, t.what, t.stories))
            .collect();
        assert_eq!(
            got,
            [
                (
                    "kernel / セルの実行".to_string(),
                    ":q は何もしない".to_string(),
                    vec!["J1".to_string(), "E3".to_string()],
                ),
                (
                    "kernel / カーネルの登録".to_string(),
                    "複数行の".to_string(),
                    vec!["J1".to_string()],
                ),
            ]
        );
    }
    #[test]
    fn 三重引用符の中の_def_は読まない() {
        let text = r#"
def helper():
    """1 行で閉じる docstring。"""

def describe_セルの実行():

    @story("J1")
    def 前のセルの宣言を使える(kernel):
        cell = """\
def fib : Nat → Nat
  | 0 => 0"""
        pass

    @story("J2")
    async def 後ろのテスト(kernel):
        pass
"#;
        let got: Vec<(String, String)> = python_tests(text, "kernel")
            .unwrap()
            .into_iter()
            .map(|t| (t.place, t.what))
            .collect();
        let place = "kernel / セルの実行".to_string();
        assert_eq!(
            got,
            [
                (place.clone(), "前のセルの宣言を使える".to_string()),
                (place, "後ろのテスト".to_string()),
            ]
        );
    }

    #[test]
    fn story_が付いているのにテストでない関数はエラーにする() {
        let file = syn::parse_file(
            r#"
            mod 式の評価 {
                #[story("A1")]
                fn 属性を付け忘れた() {}
            }
            "#,
        )
        .unwrap();
        assert!(descend(&file.items, "parsing", None, Path::new(".")).is_err());
    }

    #[test]
    fn 入れ子の_mod_は外側の名前から並べる() {
        let file = syn::parse_file(
            r#"
            mod 外側 {
                mod 内側 {
                    #[story("A1")]
                    #[test]
                    fn 評価される() {}
                }
            }
            "#,
        )
        .unwrap();
        let got: Vec<String> = descend(&file.items, "parsing", None, Path::new("."))
            .unwrap()
            .into_iter()
            .map(|t| t.place)
            .collect();
        assert_eq!(got, ["parsing / 外側 / 内側"]);
    }

    #[test]
    fn コメントの中の三重引用符では文字列にしない() {
        let text = r#"
def describe_セルの実行():

    @story("J1")
    def 前のテスト(kernel):
        # 次の """ は文字列ではない
        pass

    @story("J2")
    def 後ろのテスト(kernel):
        pass
"#;
        let got: Vec<String> = python_tests(text, "kernel")
            .unwrap()
            .into_iter()
            .map(|t| t.what)
            .collect();
        assert_eq!(got, ["前のテスト", "後ろのテスト"]);
    }

    #[test]
    fn describe_の外か_class_の中で_story_を付けるとエラーにする() {
        let top = "@story(\"J1\")\ndef 外のテスト(kernel):\n    pass\n";
        assert!(python_tests(top, "kernel").is_err());
        let class = "def describe_セル():\n    class TestX:\n        @story(\"J1\")\n        def 中のテスト(self):\n            pass\n";
        assert!(python_tests(class, "kernel").is_err());
    }

    #[test]
    fn ふつうの文字列の中の三重引用符では文字列にしない() {
        let text = r#"
def describe_セルの実行():

    @story("J1")
    def 前のテスト(kernel):
        src = 'x = """'
        code = "theorem h''' : True := trivial"
        doc = """閉じる""" + """
続く"""
        pass

    @story("J2")
    def 後ろのテスト(kernel):
        pass
"#;
        let got: Vec<String> = python_tests(text, "kernel")
            .unwrap()
            .into_iter()
            .map(|t| t.what)
            .collect();
        assert_eq!(got, ["前のテスト", "後ろのテスト"]);
    }

    #[test]
    fn 入れ子の_describe_は外側の名前から並べる() {
        let text = r#"
def describe_外側A():
    def describe_共通():
        @story("J1")
        def 同じ名前(kernel):
            pass

def describe_外側B():
    def describe_共通():
        @story("J1")
        def 同じ名前(kernel):
            pass
"#;
        let got: Vec<String> = python_tests(text, "kernel")
            .unwrap()
            .into_iter()
            .map(|t| t.place)
            .collect();
        assert_eq!(got, ["kernel / 外側A / 共通", "kernel / 外側B / 共通"]);
    }

    /// テストごとの一時ディレクトリ。`files` の (パス, 中身) を書いておく。
    fn scratch(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spec-{}-{name}", std::process::id()));
        for (path, text) in files {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        dir
    }

    #[test]
    fn 別のファイルの_mod_のテストも読む() {
        let test = "#[story(\"A1\")]\n#[test]\nfn 評価される() {}\n";
        let dir = scratch(
            "mod",
            &[
                ("外.rs", "mod 内;\n"),
                ("外/内.rs", test),
                ("共通/mod.rs", "mod 下;\n"),
                ("共通/下.rs", test),
            ],
        );
        let file = syn::parse_file("mod 外;\nmod 共通;\n").unwrap();
        let got = descend(&file.items, "parsing", None, &dir)
            .map(|v| v.into_iter().map(|t| t.place).collect::<Vec<_>>());
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(got.unwrap(), ["parsing / 外 / 内", "parsing / 共通 / 下"]);
    }

    #[test]
    fn 実行されないか読み取れないテストはエラーにする() {
        let dir = std::env::temp_dir();
        for text in [
            "proptest! { #[test] fn 評価される(a in 0..1) {} }",
            "#[story(\"A1\")] #[test] #[ignore] fn 評価される() {}",
            "#[story(\"A1\")] #[test] #[cfg(unix)] fn 評価される() {}",
            "#[cfg(unix)] mod 式 { #[story(\"A1\")] #[test] fn 評価される() {} }",
            "mod 無いファイル;",
        ] {
            let file = syn::parse_file(text).unwrap();
            assert!(
                descend(&file.items, "parsing", None, &dir).is_err(),
                "{text}"
            );
        }
    }

    #[test]
    fn stories_toml_の誤りと同じ名前のテストを見つける() {
        let stories: Stories = toml::from_str(
            r#"
unbuilt = ["A2"]
[[group]]
id = "A"
title = "式"
[[group]]
id = "A"
title = "式"
[[story]]
id = "A1"
text = "a"
[[story]]
id = "A1"
text = "a"
[[story]]
id = "A2"
text = "b"
[[story]]
id = "Z1"
text = "z"
"#,
        )
        .unwrap();
        let test = |what: &str, sid: &str| Test {
            what: what.to_string(),
            place: "kernel / 補完".to_string(),
            stories: vec![sid.to_string()],
        };
        let tests = [test("同じ", "A1"), test("同じ", "A1"), test("別", "A2")];
        assert_eq!(
            complaints(&stories, &tests),
            [
                "story の番号が重複している: A1",
                "group の番号が重複している: A",
                "どの group にも入らない: Z1",
                "unbuilt にあるのにテストがある: A2",
                "同じ名前のテストがある: kernel / 補完 / 同じ",
            ]
        );
    }
}
