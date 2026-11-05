//! テストとユーザーストーリーから `SPEC.md` を生成する。手で書いた一覧は必ずコードより
//! 古くなるので、`SPEC.md` に手で書き足す場所は作らない。
//!
//! ```text
//! stories.toml                誰の何を助けるか。コードからは分からないので、ここが情報源
//! crates/leani/tests/*.rs     それを確かめているテスト。doc の ja: の行が仕様の文になる
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
//! describe と見なす。テストの文は関数の doc comment (`///`) の `ja:` の行から読み、describe の
//! 文は `mod` の中の doc comment (`//!`) の `ja:` の行から読む。
//!
//! Python のテストは行単位で読む。`def describe_` の行をインデントで入れ子にし、その中で
//! `@story(...)` の行の次にある `def` をテストと見なす。describe とテストの文は、`def` の
//! すぐ後ろの docstring の `ja:` の行から読む。Python の構文を解析する crate は使わない。
//! カーネルのテストは数が少なく、書き方も決まっているので、行単位で足りる。
//!
//! 文は `ja:` の後ろに 1 行で書く。`ja:` の次の行に続きを書くと、続きを読み落とすので
//! エラーにする。doc のほかの行には、テストの説明を自由に書いてよい。

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use syn::punctuated::Punctuated;
use syn::{AttrStyle, Attribute, Expr, ExprLit, Item, Lit, LitStr, Meta, Token};

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
    pub ja: String,
}

#[derive(Deserialize)]
pub struct Story {
    pub id: String,
    pub ja: String,
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

// ------------------------------------------------------------------ 文を読む

/// doc の `ja:` と `en:` の行に書いた文。
#[derive(Default)]
struct Text {
    ja: Option<String>,
    en: Option<String>,
}

/// doc の行から `ja:` と `en:` の文を読む。`at` はエラーに出す場所。
///
/// 同じ印の行が 2 つあるとき、印の行の次の行が空行でも印の行でもないときはエラーにする。
/// 後者は、文を 2 行に分けて書いたときに 2 行目を読み落とさないため。
fn text(lines: &[String], at: &str) -> Result<Text, String> {
    let (text, _) = lines
        .iter()
        .try_fold((Text::default(), false), |(text, after), line| {
            let line = line.trim();
            match (line.strip_prefix("ja:"), line.strip_prefix("en:")) {
                (Some(ja), _) => match text.ja {
                    Some(_) => Err(format!("ja: が 2 つある: {at}")),
                    None => Ok((
                        Text {
                            ja: Some(ja.trim().to_string()),
                            ..text
                        },
                        true,
                    )),
                },
                (_, Some(en)) => match text.en {
                    Some(_) => Err(format!("en: が 2 つある: {at}")),
                    None => Ok((
                        Text {
                            en: Some(en.trim().to_string()),
                            ..text
                        },
                        true,
                    )),
                },
                _ if after && !line.is_empty() => {
                    Err(format!("ja: か en: の次の行に続きがある: {at}"))
                }
                _ => Ok((text, false)),
            }
        })?;
    Ok(text)
}

/// doc の `ja:` の文。無いか空ならエラー。
fn ja(lines: &[String], at: &str) -> Result<String, String> {
    match text(lines, at)?.ja {
        Some(ja) if !ja.is_empty() => Ok(ja),
        _ => Err(format!("doc に ja: の文が無い: {at}")),
    }
}

/// doc comment の行。`inner` なら `//!` を、そうでなければ `///` を読む。
///
/// syn は、`mod` の外に書いた `///` と中に書いた `//!` を同じ `attrs` に入れるので、
/// 書いた場所で分ける。
fn doc_lines(attrs: &[Attribute], inner: bool) -> Vec<String> {
    attrs
        .iter()
        .filter(|a| matches!(a.style, AttrStyle::Inner(_)) == inner && a.path().is_ident("doc"))
        .filter_map(|a| match &a.meta {
            Meta::NameValue(nv) => match &nv.value {
                Expr::Lit(ExprLit {
                    lit: Lit::Str(s), ..
                }) => Some(s.value()),
                _ => None,
            },
            _ => None,
        })
        // 空の `///` は空の文字列になり、`lines` では 0 行になるので、`split` で分ける。
        .flat_map(|s| s.split('\n').map(str::to_string).collect::<Vec<_>>())
        .collect()
}

// ------------------------------------------------------------ テストを集める

/// テスト 1 件。`name` は関数名、`what` は doc の `ja:` の文、`place` は「層 / describe」。
pub struct Test {
    pub name: String,
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

/// 外側の describe の文と内側の場所を ` / ` でつなぐ。内側が空なら外側だけ。
fn nest(outer: &str, inner: &str) -> String {
    match inner {
        "" => outer.to_string(),
        _ => format!("{outer} / {inner}"),
    }
}

/// Rust のテストファイル 1 つのテスト。場所は「層 / describe」で、`mod` の外のテストは
/// describe の代わりに層の名前を書く。`dir` は、`mod x;` と書いたときに `x` のファイルを
/// 探すディレクトリ。
fn rust_tests(items: &[Item], layer: &str, dir: &Path) -> Result<Vec<Test>, String> {
    Ok(descend(items, layer, dir)?
        .into_iter()
        .map(|t| {
            let place = format!(
                "{layer} / {}",
                match t.place.as_str() {
                    "" => layer,
                    p => p,
                }
            );
            Test { place, ..t }
        })
        .collect())
}

/// `mod` を再帰的にたどってテストを集める。テストを含む `mod` が describe になる。
/// `mod` は入れ子にでき、返すテストの場所は、`items` の中の describe の文を ` / ` で
/// つないだもの。`at` は、エラーに出すための `mod` の名前の並び。
///
/// 次の書き方はエラーにする。どれも、`SPEC.md` と実行されるテストがずれるのに気づけない。
///
/// ```text
/// #[story] が付いているのにテストの属性が無い関数   実行されないのに載る
/// #[ignore] か #[cfg] が付いたテストと mod         実行されないことがあるのに載る
/// 直下のマクロ (proptest! など)                   中のテストを読めないので載らない
/// #[path] の付いた mod                            ファイルを探さないので載らない
/// 同じ文の mod が並ぶ                             2 つの mod のテストが 1 つの describe に載る
/// ```
fn descend(items: &[Item], at: &str, dir: &Path) -> Result<Vec<Test>, String> {
    let found = items
        .iter()
        .map(|item| item_tests(item, at, dir))
        .collect::<Result<Vec<_>, String>>()?;
    let mods = repeated(found.iter().filter_map(|(m, _)| m.as_deref()));
    match mods.first() {
        Some(same) => Err(format!("同じ文の mod が並んでいる: {at} / {same}")),
        None => Ok(found.into_iter().flat_map(|(_, tests)| tests).collect()),
    }
}

/// 1 つの item のテスト。item がテストを含む `mod` なら、その文も返す。
fn item_tests(item: &Item, at: &str, dir: &Path) -> Result<(Option<String>, Vec<Test>), String> {
    match item {
        Item::Mod(m) if m.attrs.iter().any(|a| a.path().is_ident("path")) => Err(format!(
            "#[path] の付いた mod は読まない: {at} / {}",
            m.ident
        )),
        Item::Mod(m) => {
            let ident = m.ident.to_string();
            let inside = format!("{at} / {ident}");
            let sub = dir.join(&ident);
            let (docs, tests) = match &m.content {
                Some((_, inner)) => (doc_lines(&m.attrs, true), descend(inner, &inside, &sub)?),
                None => {
                    let file = mod_file(dir, &ident)?;
                    (
                        doc_lines(&file.attrs, true),
                        descend(&file.items, &inside, &sub)?,
                    )
                }
            };
            match (skipped(&m.attrs), tests.is_empty()) {
                (true, false) => Err(format!("実行されないことがあるテスト: {inside}")),
                (_, true) => Ok((None, tests)),
                (false, false) => {
                    let name = ja(&docs, &format!("mod {inside}"))?;
                    let tests = tests
                        .into_iter()
                        .map(|t| Test {
                            place: nest(&name, &t.place),
                            ..t
                        })
                        .collect();
                    Ok((Some(name), tests))
                }
            }
        }
        Item::Macro(m) if m.ident.is_none() => Err(format!(
            "マクロの中のテストは読めない: {at} / {}!",
            m.mac
                .path
                .segments
                .last()
                .map_or(String::new(), |s| s.ident.to_string())
        )),
        Item::Fn(f) if is_test(&f.attrs) && skipped(&f.attrs) => Err(format!(
            "実行されないことがあるテスト: {at} / {}",
            f.sig.ident
        )),
        Item::Fn(f) if is_test(&f.attrs) => {
            let name = f.sig.ident.to_string();
            let what = ja(&doc_lines(&f.attrs, false), &format!("{at} / {name}"))?;
            Ok((
                None,
                vec![Test {
                    name,
                    what,
                    place: String::new(),
                    stories: marked(&f.attrs),
                }],
            ))
        }
        Item::Fn(f) if !marked(&f.attrs).is_empty() => Err(format!(
            "テストでない関数に #[story] が付いている: {at} / {}",
            f.sig.ident
        )),
        _ => Ok((None, Vec::new())),
    }
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

/// Python のブロックの種類。
enum Scope {
    /// ファイルの直下。
    Top,
    /// `def describe_`。中身は describe の文。
    Describe(String),
    /// `class`。pytest-describe は中の関数を集めない。
    Class,
    /// ほかの `def`。テストの中の `def` を、describe の直下の `def` と区別するために数える。
    Function,
}

/// 開いているブロック。`names` は直下の `def` と `class` の名前、`describes` は直下の
/// describe の文。どちらも重複を見つけるために持つ。
struct Frame {
    indent: Option<usize>,
    scope: Scope,
    names: Vec<String>,
    describes: Vec<String>,
}

impl Frame {
    fn new(indent: Option<usize>, scope: Scope) -> Self {
        Frame {
            indent,
            scope,
            names: Vec::new(),
            describes: Vec::new(),
        }
    }
}

/// docstring を待っている `def`。
enum Waiting {
    Describe {
        name: String,
        indent: usize,
    },
    Test {
        name: String,
        place: String,
        stories: Vec<String>,
    },
}

impl Waiting {
    fn name(&self) -> &str {
        match self {
            Waiting::Describe { name, .. } | Waiting::Test { name, .. } => name,
        }
    }
}

/// docstring を読んでいるところ。
enum Doc {
    Off,
    /// `def` の行を読んだが、docstring はまだ始まっていない。
    Before(Waiting),
    /// docstring の中。読んだ行をためる。
    Inside(Waiting, Vec<String>),
}

struct Scan {
    frames: Vec<Frame>,
    /// 次の `def` に付く `@story` の番号。
    pending: Vec<String>,
    /// 開いている三重引用符。閉じるまで行を読まない。
    string: Option<&'static str>,
    doc: Doc,
    out: Vec<Test>,
}

/// `"""` だけの行か。describe とテストの docstring は、この行で始めてこの行で閉じる。
fn quotes(body: &str) -> bool {
    body.trim_end() == "\"\"\""
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
///
/// 同じブロックの直下に同じ名前の `def` が 2 つあるとエラーにする。後の `def` が前の
/// テストや describe を隠すので、pytest は前のものを集めないのに、ここでは載せてしまう。
fn python_tests(text: &str, layer: &str) -> Result<Vec<Test>, String> {
    let start = Scan {
        frames: vec![Frame::new(None, Scope::Top)],
        pending: Vec::new(),
        string: None,
        doc: Doc::Off,
        out: Vec::new(),
    };
    let done = text
        .lines()
        .try_fold(start, |scan, line| python_line(scan, line, layer))?;
    match done.doc {
        Doc::Off => Ok(done.out),
        Doc::Before(w) | Doc::Inside(w, _) => {
            Err(format!("docstring が閉じていない: {layer} / {}", w.name()))
        }
    }
}

/// Python の 1 行を読む。
fn python_line(mut scan: Scan, line: &str, layer: &str) -> Result<Scan, String> {
    let body = line.trim_start();
    match std::mem::replace(&mut scan.doc, Doc::Off) {
        Doc::Inside(w, lines) if quotes(body) => return finish(scan, w, &lines, layer),
        Doc::Inside(w, lines) => {
            scan.doc = Doc::Inside(w, lines.into_iter().chain([body.to_string()]).collect());
            return Ok(scan);
        }
        Doc::Before(w) if quotes(body) => {
            scan.doc = Doc::Inside(w, Vec::new());
            return Ok(scan);
        }
        Doc::Before(w)
            if ["def ", "async def ", "@", "class "]
                .iter()
                .any(|p| body.starts_with(p)) =>
        {
            return Err(format!(
                "def のすぐ後ろに ja: の docstring が無い: {layer} / {}",
                w.name()
            ));
        }
        Doc::Before(w) => {
            scan.doc = Doc::Before(w);
            return Ok(scan);
        }
        Doc::Off => {}
    }
    let inside = scan.string;
    scan.string = string_after(line, inside);
    if inside.is_some() {
        return Ok(scan);
    }
    let indent = line.len() - body.len();
    if body.starts_with("@story(") {
        scan.pending
            .extend(body.split('"').skip(1).step_by(2).map(str::to_string));
        return Ok(scan);
    }
    let def = body
        .strip_prefix("async ")
        .unwrap_or(body)
        .strip_prefix("def ")
        .and_then(|rest| rest.split('(').next());
    let class = body
        .strip_prefix("class ")
        .and_then(|rest| rest.split(['(', ':']).next());
    let Some(name) = def.or(class).map(str::trim) else {
        return Ok(scan);
    };
    scan.frames
        .retain(|f| f.indent.is_none_or(|at| at < indent));
    let parent = scan.frames.len() - 1;
    if scan.frames[parent].names.iter().any(|n| n == name) {
        return Err(format!("同じ場所に同じ名前の def がある: {layer} / {name}"));
    }
    scan.frames[parent].names.push(name.to_string());
    let stories = std::mem::take(&mut scan.pending);
    if class.is_some() {
        scan.frames.push(Frame::new(Some(indent), Scope::Class));
        return Ok(scan);
    }
    if name.starts_with("describe_") {
        if !stories.is_empty() {
            return Err(format!("describe に @story が付いている: {layer} / {name}"));
        }
        scan.doc = Doc::Before(Waiting::Describe {
            name: name.to_string(),
            indent,
        });
        return Ok(scan);
    }
    let names: Option<Vec<&str>> = scan.frames[1..]
        .iter()
        .map(|f| match &f.scope {
            Scope::Describe(d) => Some(d.as_str()),
            Scope::Top | Scope::Class | Scope::Function => None,
        })
        .collect();
    let place = names.filter(|n| !n.is_empty()).map(|n| n.join(" / "));
    scan.frames.push(Frame::new(Some(indent), Scope::Function));
    if stories.is_empty() {
        return Ok(scan);
    }
    let Some(place) = place else {
        return Err(format!(
            "describe の外のテストは pytest-describe が実行しない: {layer} / {name}"
        ));
    };
    scan.doc = Doc::Before(Waiting::Test {
        name: name.to_string(),
        place: format!("{layer} / {place}"),
        stories,
    });
    Ok(scan)
}

/// docstring を閉じたときに、待っていた `def` を describe かテストにする。
fn finish(mut scan: Scan, w: Waiting, lines: &[String], layer: &str) -> Result<Scan, String> {
    let what = ja(lines, &format!("{layer} / {}", w.name()))?;
    match w {
        Waiting::Describe { indent, .. } => {
            let parent = scan.frames.len() - 1;
            if scan.frames[parent].describes.contains(&what) {
                return Err(format!(
                    "同じ場所に同じ文の describe がある: {layer} / {what}"
                ));
            }
            scan.frames[parent].describes.push(what.clone());
            scan.frames
                .push(Frame::new(Some(indent), Scope::Describe(what)));
        }
        Waiting::Test {
            name,
            place,
            stories,
        } => scan.out.push(Test {
            name,
            what,
            place,
            stories,
        }),
    }
    Ok(scan)
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
            rust_tests(&file.items, layer, dir)
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
/// `stories.toml` の誤り、同じ場所の同じ文のテスト。
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
        .map(|p| format!("同じ文のテストがある: {p}"));
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
    format!("### {} {}\n\n{body}", story.id, story.ja)
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
                std::iter::once(format!("## {}. {}\n", g.id, g.ja))
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

    /// (場所, 文, 識別子, ストーリー)
    type Row = (String, String, String, Vec<String>);

    /// Rust のソースを読んで、(場所, 文, 識別子, ストーリー) を返す。
    fn rust(text: &str) -> Result<Vec<Row>, String> {
        let file = syn::parse_file(text).unwrap();
        rust_tests(&file.items, "parsing", Path::new(".")).map(|v| {
            v.into_iter()
                .map(|t| (t.place, t.what, t.name, t.stories))
                .collect()
        })
    }

    /// Python のソースを読んで、(場所, 文, 識別子, ストーリー) を返す。
    fn python(text: &str) -> Result<Vec<Row>, String> {
        python_tests(text, "kernel").map(|v| {
            v.into_iter()
                .map(|t| (t.place, t.what, t.name, t.stories))
                .collect()
        })
    }

    fn row(place: &str, what: &str, name: &str, sids: &[&str]) -> Row {
        (
            place.to_string(),
            what.to_string(),
            name.to_string(),
            sids.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn テストの属性が付いた関数だけを_doc_の_ja_の文で集める() {
        let got = rust(
            r#"
            mod eval {
                //! ja: 式の評価
                //!
                //! mod の説明。
                fn helper() {}
                /// ja: 単独の式を評価する
                #[story("A1")]
                #[test]
                fn single() {}
                /// ja: どう連結しても閉じている
                /// en: Stays balanced
                ///
                /// テストの説明。
                #[story("E2")]
                #[proptest]
                fn concat(a: String) {}
            }
            mod common {
                pub fn drive() {}
            }
            "#,
        );
        assert_eq!(
            got.unwrap(),
            [
                row(
                    "parsing / 式の評価",
                    "単独の式を評価する",
                    "single",
                    &["A1"]
                ),
                row(
                    "parsing / 式の評価",
                    "どう連結しても閉じている",
                    "concat",
                    &["E2"]
                ),
            ]
        );
    }

    #[test]
    fn mod_の文は中の_doc_だけから読む() {
        let got = rust(
            r#"
            /// ja: 外の文
            mod eval {
                //! ja: 中の文
                /// ja: 評価する
                #[story("A1")]
                #[test]
                fn t() {}
            }
            "#,
        );
        assert_eq!(
            got.unwrap(),
            [row("parsing / 中の文", "評価する", "t", &["A1"])]
        );
    }

    #[test]
    fn doc_の_ja_が無いか_2_つあるか続きの行があればエラーにする() {
        for doc in [
            "",
            "/// en: Evaluates",
            "/// ja: 評価する\n/// ja: 評価する",
            "/// ja: 評価\n/// する",
            "/// ja:",
        ] {
            let text =
                format!("mod m {{ //! ja: 式\n{doc}\n#[story(\"A1\")] #[test] fn t() {{}} }}");
            assert!(rust(&text).is_err(), "{doc}");
        }
        let no_mod_doc = "mod m { /// ja: 評価する\n#[story(\"A1\")] #[test] fn t() {} }";
        assert!(rust(no_mod_doc).is_err());
    }

    #[test]
    fn 同じ_mod_の中に同じ文の_mod_があればエラーにする() {
        let text = r#"
            mod a { //! ja: 式
                /// ja: 評価する
                #[story("A1")] #[test] fn t() {} }
            mod b { //! ja: 式
                /// ja: 表示する
                #[story("A1")] #[test] fn u() {} }
        "#;
        assert!(rust(text).is_err());
    }

    #[test]
    fn describe_の中で_story_を付けた関数を_docstring_の_ja_の文で読む() {
        let text = r#"
def describe_cell():
    """
    ja: セルの実行
    """

    @story("J1", "E3")
    @pytest.mark.usefixtures("place")
    def quit(kernel):
        """
        ja: :q は何もしない
        en: Does nothing

        テストの説明。
        """
        pass

    @pytest.fixture
    def helper():
        pass

    def 補助():
        pass

def describe_register():
    """
    ja: カーネルの登録
    """
    # コメント

    @story("J1")
    @pytest.mark.parametrize(
        "exc",
        [1, 2],
    )
    def multi(
        tmp_path,
        exc,
    ):
        # 引数のあとのコメント
        """
        ja: 複数行の引数
        """
        pass
"#;
        assert_eq!(
            python(text).unwrap(),
            [
                row(
                    "kernel / セルの実行",
                    ":q は何もしない",
                    "quit",
                    &["J1", "E3"]
                ),
                row("kernel / カーネルの登録", "複数行の引数", "multi", &["J1"]),
            ]
        );
    }

    #[test]
    fn docstring_より先に_def_が来るか_ja_が無ければエラーにする() {
        let before = "def describe_cell():\n    @story(\"J1\")\n    def t(kernel):\n        \"\"\"\n        ja: 文\n        \"\"\"\n";
        assert!(python(before).is_err());
        let test = "def describe_cell():\n    \"\"\"\n    ja: セル\n    \"\"\"\n    @story(\"J1\")\n    def t(kernel):\n        pass\n";
        assert!(python(test).is_err());
        let no_ja = "def describe_cell():\n    \"\"\"\n    セル\n    \"\"\"\n";
        assert!(python(no_ja).is_err());
        let open = "def describe_cell():\n    \"\"\"\n    ja: セル\n";
        assert!(python(open).is_err());
    }

    #[test]
    fn 同じ_scope_に同じ名前の_def_があればエラーにする() {
        let doc = |ja: &str, pad: &str| format!("{pad}\"\"\"\n{pad}ja: {ja}\n{pad}\"\"\"\n");
        let test = |name: &str, ja: &str| {
            format!(
                "    @story(\"J1\")\n    def {name}(kernel):\n{}",
                doc(ja, "        ")
            )
        };
        let fixture = "    @pytest.fixture\n    def t():\n        pass\n";
        let head = format!("def describe_cell():\n{}", doc("セル", "    "));
        for body in [
            format!("{}{}", test("t", "前"), test("t", "後ろ")),
            format!("{}{fixture}", test("t", "前")),
        ] {
            assert!(python(&format!("{head}{body}")).is_err(), "{body}");
        }
        let same_describe = format!("{head}{}{head}{}", test("a", "前"), test("b", "後ろ"));
        assert!(python(&same_describe).is_err());
        let other_scope = format!(
            "{head}{}    def helper():\n        pass\n{}",
            test("t", "前"),
            "    def other():\n        def helper():\n            pass\n"
        );
        assert!(python(&other_scope).is_ok());
    }

    #[test]
    fn 三重引用符の中の_def_は読まない() {
        let text = r#"
def helper():
    """1 行で閉じる docstring。"""

def describe_cell():
    """
    ja: セルの実行
    """

    @story("J1")
    def before(kernel):
        """
        ja: 前のセルの宣言を使える
        """
        cell = """\
def fib : Nat → Nat
  | 0 => 0"""
        pass

    @story("J2")
    async def after(kernel):
        """
        ja: 後ろのテスト
        """
        pass
"#;
        assert_eq!(
            python(text).unwrap(),
            [
                row(
                    "kernel / セルの実行",
                    "前のセルの宣言を使える",
                    "before",
                    &["J1"]
                ),
                row("kernel / セルの実行", "後ろのテスト", "after", &["J2"]),
            ]
        );
    }

    #[test]
    fn story_が付いているのにテストでない関数はエラーにする() {
        let text = "mod m { //! ja: 式\n/// ja: 評価する\n#[story(\"A1\")] fn t() {} }";
        assert!(rust(text).is_err());
    }

    #[test]
    fn 入れ子の_mod_は外側の文から並べる() {
        let text = r#"
            mod outer {
                //! ja: 外側
                mod inner {
                    //! ja: 内側
                    /// ja: 評価する
                    #[story("A1")]
                    #[test]
                    fn t() {}
                }
            }
        "#;
        let got: Vec<String> = rust(text).unwrap().into_iter().map(|t| t.0).collect();
        assert_eq!(got, ["parsing / 外側 / 内側"]);
    }

    /// describe とテスト 2 件。`body` は 1 件目のテストの本文。
    fn two_tests(body: &str) -> String {
        format!(
            "def describe_cell():\n    \"\"\"\n    ja: セル\n    \"\"\"\n\n    @story(\"J1\")\n    def before(kernel):\n        \"\"\"\n        ja: 前のテスト\n        \"\"\"\n{body}\n\n    @story(\"J2\")\n    def after(kernel):\n        \"\"\"\n        ja: 後ろのテスト\n        \"\"\"\n        pass\n"
        )
    }

    fn whats(text: &str) -> Vec<String> {
        python(text).unwrap().into_iter().map(|t| t.1).collect()
    }

    #[test]
    fn コメントの中の三重引用符では文字列にしない() {
        let text = two_tests("        # 次の \"\"\" は文字列ではない\n        pass");
        assert_eq!(whats(&text), ["前のテスト", "後ろのテスト"]);
    }

    #[test]
    fn describe_の外か_class_の中で_story_を付けるとエラーにする() {
        let doc = "    \"\"\"\n    ja: 文\n    \"\"\"\n";
        let top = format!("@story(\"J1\")\ndef t(kernel):\n{doc}");
        assert!(python(&top).is_err());
        let class = format!(
            "def describe_cell():\n{doc}    class TestX:\n        @story(\"J1\")\n        def t(self):\n            \"\"\"\n            ja: 文\n            \"\"\"\n"
        );
        assert!(python(&class).is_err());
    }

    #[test]
    fn ふつうの文字列の中の三重引用符では文字列にしない() {
        let text = two_tests(
            "        src = 'x = \"\"\"'\n        code = \"theorem h''' : True := trivial\"\n        doc = \"\"\"閉じる\"\"\" + \"\"\"\n続く\"\"\"\n        pass",
        );
        assert_eq!(whats(&text), ["前のテスト", "後ろのテスト"]);
    }

    #[test]
    fn 入れ子の_describe_は外側の文から並べる() {
        let text = r#"
def describe_a():
    """
    ja: 外側A
    """
    def describe_common():
        """
        ja: 共通
        """
        @story("J1")
        def same(kernel):
            """
            ja: 同じ文
            """
            pass

def describe_b():
    """
    ja: 外側B
    """
    def describe_common():
        """
        ja: 共通
        """
        @story("J1")
        def same(kernel):
            """
            ja: 同じ文
            """
            pass
"#;
        let got: Vec<String> = python(text).unwrap().into_iter().map(|t| t.0).collect();
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
    fn 別のファイルの_mod_のテストはそのファイルの_doc_の文で読む() {
        let test = "/// ja: 評価する\n#[story(\"A1\")]\n#[test]\nfn t() {}\n";
        let dir = scratch(
            "mod",
            &[
                ("outer.rs", "//! ja: 外\nmod inner;\n"),
                ("outer/inner.rs", &format!("//! ja: 内\n{test}")),
                ("common/mod.rs", "//! ja: 共通\nmod sub;\n"),
                ("common/sub.rs", &format!("//! ja: 下\n{test}")),
            ],
        );
        let file = syn::parse_file("mod outer;\nmod common;\n").unwrap();
        let got = rust_tests(&file.items, "parsing", &dir)
            .map(|v| v.into_iter().map(|t| t.place).collect::<Vec<_>>());
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(got.unwrap(), ["parsing / 外 / 内", "parsing / 共通 / 下"]);
    }

    #[test]
    fn 実行されないか読み取れないテストはエラーにする() {
        let dir = std::env::temp_dir();
        for text in [
            "proptest! { #[test] fn t(a in 0..1) {} }",
            "/// ja: 評価する\n#[story(\"A1\")] #[test] #[ignore] fn t() {}",
            "/// ja: 評価する\n#[story(\"A1\")] #[test] #[cfg(unix)] fn t() {}",
            "#[cfg(unix)] mod m { //! ja: 式\n/// ja: 評価する\n#[story(\"A1\")] #[test] fn t() {} }",
            "mod missing;",
        ] {
            let file = syn::parse_file(text).unwrap();
            assert!(rust_tests(&file.items, "parsing", &dir).is_err(), "{text}");
        }
    }

    #[test]
    fn stories_toml_の誤りと同じ文のテストを見つける() {
        let stories: Stories = toml::from_str(
            r#"
unbuilt = ["A2"]
[[group]]
id = "A"
ja = "式"
[[group]]
id = "A"
ja = "式"
[[story]]
id = "A1"
ja = "a"
[[story]]
id = "A1"
ja = "a"
[[story]]
id = "A2"
ja = "b"
[[story]]
id = "Z1"
ja = "z"
"#,
        )
        .unwrap();
        let test = |what: &str, sid: &str| Test {
            name: "t".to_string(),
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
                "同じ文のテストがある: kernel / 補完 / 同じ",
            ]
        );
    }
}
