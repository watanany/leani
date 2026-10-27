//! テストとユーザーストーリーから `SPEC.md` を生成する。
//!
//! leani で「何ができて、どういう性質を持つか」の一覧が必要。コードから導かずに書くと
//! 必ずコードより古くなるので、情報源を 2 つだけに絞って生成する。手で書き足す場所は作らない。
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
//! ストーリーとテストは 1:1 ではない。テストの分け方はテストの都合で決まっていて
//! (失敗する理由が 1 つになるように、なるべく速い層でテストするように)、ストーリーの
//! 分け方とは一致しない。なので各テストに `#[story(...)]` を付けてもらい、ここで対応を
//! 取る。テストが 0 件のストーリーには「確かめているテストが無い。」と表示する
//! (`unbuilt` にあるストーリーには「まだ作っていない。」と表示する)。
//! それが分かることがこの表の目的。
//!
//! Rust のテストには、Python 版の `it_` と `describe_` を付けない。`#[test]` か `#[proptest]`
//! の付いた関数をテストと見なし、テストを含む `mod` を describe と見なす。
//!
//! Python のテストは行単位で読む。`def describe_` と `def it_` の行をインデントで入れ子に
//! し、その前の `@story(...)` の行からストーリー番号を取る。名前からは `it_` と
//! `describe_` を外す。Python の構文を解析する crate は使わない。カーネルのテストは
//! 数が少なく、書き方も決まっているので、行単位で足りる。
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

/// `stories.toml` の中身。`group` と `story` は書いた順に並ぶ。
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

/// `mod` を再帰的にたどってテストを集める。テストを含む `mod` が describe になる。
/// `mod` は入れ子にできる。
fn descend(items: &[Item], layer: &str, place: &str) -> Vec<Test> {
    items
        .iter()
        .flat_map(|item| match item {
            Item::Mod(m) => match &m.content {
                Some((_, inner)) => descend(inner, layer, &label(&m.ident.to_string())),
                None => Vec::new(),
            },
            Item::Fn(f) if is_test(&f.attrs) => vec![Test {
                what: label(&f.sig.ident.to_string()),
                place: format!("{layer} / {place}"),
                stories: marked(&f.attrs),
            }],
            _ => Vec::new(),
        })
        .collect()
}

/// Python のテストファイルを行単位で読む。describe はインデントで入れ子にする。
fn python_tests(text: &str, layer: &str) -> Vec<Test> {
    struct Scan {
        /// 開いている describe の (インデント, 名前)。
        describes: Vec<(usize, String)>,
        /// 次の `def` に付く `@story` の番号。
        pending: Vec<String>,
        out: Vec<Test>,
    }
    let start = Scan {
        describes: Vec::new(),
        pending: Vec::new(),
        out: Vec::new(),
    };
    let done = text.lines().fold(start, |mut scan, line| {
        let body = line.trim_start();
        let indent = line.len() - body.len();
        let name = body
            .strip_prefix("def ")
            .and_then(|rest| rest.split('(').next());
        if body.starts_with("@story(") {
            scan.pending
                .extend(body.split('"').skip(1).step_by(2).map(str::to_string));
            return scan;
        }
        let Some(name) = name else {
            return scan;
        };
        scan.describes.retain(|(at, _)| *at < indent);
        let stories = std::mem::take(&mut scan.pending);
        if let Some(d) = name.strip_prefix("describe_") {
            scan.describes.push((indent, label(d)));
        } else if let Some(it) = name.strip_prefix("it_") {
            let place = scan.describes.last().map_or(layer, |(_, d)| d.as_str());
            scan.out.push(Test {
                what: label(it),
                place: format!("{layer} / {place}"),
                stories,
            });
        }
        scan
    });
    done.out
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
                return Ok(python_tests(&text, layer));
            }
            let file = syn::parse_file(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(descend(&file.items, layer, layer))
        })
        .collect::<Result<Vec<_>, String>>()
        .map(|v| v.into_iter().flatten().collect())
}

/// 生成を止める理由。`#[story]` の付け忘れと、ストーリー番号の書き間違い。
pub fn complaints(stories: &Stories, tests: &[Test]) -> Vec<String> {
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
        let got: Vec<(String, String, Vec<String>)> = descend(&file.items, "parsing", "parsing")
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
    fn テストを行単位で読む() {
        let text = r#"
def describe_セルの実行():

    @story("J1", "E3")
    @pytest.mark.usefixtures("place")
    def it_colon_q_は何もしない(kernel):
        pass

    def helper():
        pass

def describe_カーネルの登録():

    @story("J1")
    def it_複数行の(
        tmp_path,
    ):
        pass
"#;
        let got: Vec<(String, String, Vec<String>)> = python_tests(text, "kernel")
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
}
