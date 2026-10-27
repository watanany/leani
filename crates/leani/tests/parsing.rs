//! 入力をどう解釈するかを決める純粋関数と、読み取りのテスト。端末もエンジンも
//! 使わないのでミリ秒で終わる。

#![allow(
    non_snake_case,
    uncommon_codepoints,
    confusable_idents,
    mixed_script_confusables
)]

use leani::{boot, search};
use leani_core::abbrev::{
    ABBREV, abbrev_candidates, abbrev_head, abbrev_lookup, abbrev_table, cell_width, expand_abbrev,
};
use leani_core::pure::{self, PAIRS, Style};
use leani_core::queries::{BLOCK_OPEN, DECL_NAME, INCOMPLETE};
use leani_core::types::{Json, Kind, Proof, SearchError, State};
use leani_read::args::{Args, parse_args};
use leani_read::config::{self, EnvConfig};
use leani_read::places::Ctx;
use proptest::prelude::*;
use serde_json::json;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;
use story::story;
use test_strategy::proptest;

// ----------------------------------------------- property test のデータ

/// 括弧が閉じている文字列。中身は Lean のコードに似せてあるが、
/// テストに必要なのは括弧の閉じ方だけ。
fn balanced_src() -> impl Strategy<Value = String> {
    let leaf = prop::sample::select(vec!["", "x", "exact rfl", "simp only"]).prop_map(String::from);
    leaf.prop_recursive(4, 8, 2, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| a + &b),
            (prop::sample::select(PAIRS.to_vec()), inner)
                .prop_map(|((open, close), x)| format!("{open}{x}{close}")),
        ]
    })
}

fn closer() -> impl Strategy<Value = char> {
    prop::sample::select(PAIRS.iter().map(|(_, close)| *close).collect::<Vec<_>>())
}

/// sorry の前後に置く行。コメントや文字列の中に "sorry" を書いた行を混ぜてある。
const DECOYS: [&str; 5] = [
    "",
    "def f : True := by",
    "-- sorry はここにもある",
    "\"sorry\"",
    "  rfl",
];

fn decoys() -> impl Strategy<Value = Vec<&'static str>> {
    prop::collection::vec(prop::sample::select(DECOYS.to_vec()), 0..=4)
}

/// エンジンが PANIC したときのメッセージ。2 行目以降はバックトレース。
const PANIC: &str = "PANIC at Foo.bar Foo:1:2: oops\nbacktrace:\n  ...";

/// ヘッダ部分のデータ。import、コメント、本体と、import に見えるが import ではない行。
const HEADERS: [&str; 11] = [
    "import Std",
    "import Lean.Elab",
    "  import Mathlib",
    "import",
    "import Foo -- メモ",
    "-- import Bar",
    "/- ここから",
    "import Baz",
    "ここまで -/",
    "",
    "def a := 1",
];

fn abbrev_keys() -> impl Strategy<Value = &'static str> {
    prop::sample::select(ABBREV.keys().map(String::as_str).collect::<Vec<_>>())
}

mod 継続行の判定 {
    //! 単独では入力になりえない行を見分ける。
    use super::*;

    #[story("C1")]
    #[test]
    fn インデントした行は前の行の続き() {
        for line in ["  | 0 => 0", "\tfoo", "  bar"] {
            assert!(pure::continues(line), "継続にならない: {line:?}");
        }
    }

    #[story("C1")]
    #[test]
    fn 行頭のパイプは前の行の続き() {
        // `|` で始まる command も tactic も Lean には無い。
        assert!(pure::continues("| zero => rfl"));
    }

    #[story("A1", "C1")]
    #[test]
    fn ふつうの行は単独の入力() {
        for line in ["def f := 1", "#eval 1", "", "x + 1"] {
            assert!(!pure::continues(line), "継続にされた: {line:?}");
        }
    }
}

mod 続きを待つ行 {
    //! 完結していても次の行がありうる構文。
    use super::*;

    #[story("C1")]
    #[test]
    fn where_with_do_by_で終わる行は空行を待つ() {
        for line in [
            "structure P where",
            "induction n with",
            "def f := do",
            "theorem t : True := by",
        ] {
            assert!(BLOCK_OPEN.is_match(line), "待たない: {line:?}");
        }
    }

    #[story("C1")]
    #[test]
    fn 語の一部として含むだけなら待たない() {
        for line in ["def where_ := 1", "#eval byte", "def f := 1"] {
            assert!(!BLOCK_OPEN.is_match(line), "余計に待つ: {line:?}");
        }
    }
}

mod パーサからの応答 {
    //! 「まだ途中」という応答だけを継続とみなす。
    use super::*;

    #[story("C1")]
    #[test]
    fn 入力の途中なら次の行を待つ() {
        assert!(INCOMPLETE.is_match("<input>:1:5: unexpected end of input"));
        assert!(INCOMPLETE.is_match("unterminated comment"));
    }

    #[story("C1")]
    #[test]
    fn ただのエラーは待たずに送る() {
        assert!(!INCOMPLETE.is_match("unknown identifier 'foo'"));
    }
}

mod タクティクの提案 {
    //! exact? や simp? は結果を "Try this:" として返す。
    use super::*;

    #[story("E2")]
    #[test]
    fn 提案から項だけを取り出す() {
        let found = pure::try_this(&[
            json!({"severity": "info", "data": "Try this:\n  [apply] exact Nat.le_refl n"}),
        ]);
        assert_eq!(found.as_deref(), Some("exact Nat.le_refl n"));
    }

    #[story("E2")]
    #[test]
    fn 折り返した提案をすべて取り出す() {
        // pretty printer が 100 桁前後で折り返す。1 行目だけ取り出すと
        // `simp only [a, b,` になり、それが完成した証明として表示されてしまう。
        let found = pure::try_this(&[json!({
            "severity": "info",
            "data": "Try this:\n  [apply] simp only [aaa, bbb,\n    ccc, ddd]",
        })]);
        assert_eq!(found.as_deref(), Some("simp only [aaa, bbb,\n  ccc, ddd]"));
    }

    #[story("E2")]
    #[test]
    fn Try_this_と同じ行に書かれた提案も取り出す() {
        let found = pure::try_this(&[json!({"data": "Try this: exact Nat.le_refl n"})]);
        assert_eq!(found.as_deref(), Some("exact Nat.le_refl n"));
    }

    #[story("E2")]
    #[test]
    fn 括弧が合わない提案は途中で切れていると判定する() {
        // スクリプトに追加する前の最後の確認。途中で切ると必ず括弧が合わなくなる。
        assert!(pure::balanced("simp only [aaa, bbb]"));
        assert!(pure::balanced("exact ⟨foo (bar x), rfl⟩"));
        assert!(!pure::balanced("simp only [aaa, bbb,"));
        assert!(!pure::balanced("exact foo)"));
        assert!(!pure::balanced("exact ⟨foo]"));
    }

    #[story("E2")]
    #[proptest]
    fn 閉じた文字列同士を連結しても閉じている(
        #[strategy(balanced_src())] a: String,
        #[strategy(balanced_src())] b: String,
        #[strategy(closer())] close: char,
    ) {
        // 例では 5 通りしか書けない。閉じた文字列を文法から生成して、連結しても
        // 閉じたままであることと、閉じ括弧を 1 つ足すと必ず閉じなくなることを確認する。
        let joined = format!("{a}{b}");
        let extra = format!("{a}{close}");
        prop_assert!(pure::balanced(&a));
        prop_assert!(pure::balanced(&joined));
        prop_assert!(!pure::balanced(&extra));
    }

    #[story("E2")]
    #[test]
    fn 提案でなければ何も返さない() {
        let done = json!({"severity": "info", "data": "goals accomplished"});
        assert_eq!(pure::try_this(&[done]), None);
        assert_eq!(pure::try_this(&[]), None);
    }
}

mod 宣言の名前の取り出し {
    //! ユーザーが実行した宣言を補完の候補に追加するために使う。
    use super::*;

    fn names(src: &str) -> Vec<&str> {
        DECL_NAME
            .captures_iter(src)
            .filter_map(|m| m.get(1).map(|g| g.as_str()))
            .collect()
    }

    #[story("D1")]
    #[test]
    fn 修飾子や属性が付いていても名前を取り出せる() {
        let cases = [
            ("def foo := 1", "foo"),
            ("theorem bar (n : Nat) : n = n := rfl", "bar"),
            ("@[simp] private noncomputable def baz : Nat := 0", "baz"),
            ("structure Point where\n  x : Nat", "Point"),
        ];
        for (src, want) in cases {
            assert_eq!(names(src), [want], "取れない: {src:?}");
        }
    }

    #[story("D1")]
    #[test]
    fn 宣言でないものからは名前を取り出さない() {
        assert!(names("#eval 1 + 1").is_empty());
    }
}

mod 補完する名前の切り出し {
    use super::*;

    #[story("D1")]
    #[test]
    fn 区切り文字の直後から名前を切り出す() {
        assert_eq!(pure::name_start("#check (Nat.suc"), "#check (".len());
        assert_eq!(pure::name_start("Nat.suc"), 0, "`.` で区切った");
    }
}

mod 補完の候補をまとめて取得する単位 {
    //! 名前空間があればそこまで、無ければ先頭 2 文字。
    use super::*;

    #[story("D1")]
    #[test]
    fn 名前空間の区切りで分ける() {
        assert_eq!(pure::name_chunk("Nat.suc"), "Nat.");
        assert_eq!(
            pure::name_chunk("MeasureTheory.integral_"),
            "MeasureTheory."
        );
    }

    #[story("D1")]
    #[test]
    fn 名前空間が無ければ先頭_2_文字() {
        // Mathlib では 1 文字だと `C` で 7.5 万件になるので、単位を広げすぎない。
        assert_eq!(pure::name_chunk("Contin"), "Co");
    }
}

mod エンジンの出力の_JSON {
    //! 問い合わせの結果はエンジンの出力の最後の行にある。
    use super::*;

    #[story("D1", "C1")]
    #[test]
    fn 出力の最後の行を_JSON_として読む() {
        let got = pure::last_json(Some("宣言が出力した行\n{\"a\": [1]}\n"));
        assert_eq!(got, Some(json!({"a": [1]})));
    }

    #[story("D1", "C1")]
    #[test]
    fn 出力が空か最後の行が_JSON_でなければ_None_を返す() {
        for out in [
            None,
            Some(""),
            Some("  \n"),
            Some("{\"a\": 1}\n途中で切れた {"),
        ] {
            assert_eq!(pure::last_json(out), None, "{out:?}");
        }
    }
}

mod 入力が完結したかの判定 {
    //! パーサの応答 (JSON) だけを見て、送り方と状態を決める。
    use super::*;

    #[story("A1", "C1")]
    #[test]
    fn command_として読めたらそのまま送る() {
        let probe = json!({"cmd": {"ok": true}, "term": {"ok": false, "err": "x"}});
        assert_eq!(pure::classify(Some(&probe)), (State::Complete, Kind::Cmd));
    }

    #[story("A1")]
    #[test]
    fn term_としてしか読めなければ_eval_に包む() {
        let probe = json!({"cmd": {"ok": false, "err": "x"}, "term": {"ok": true}});
        assert_eq!(pure::classify(Some(&probe)), (State::Complete, Kind::Term));
    }

    #[story("C1")]
    #[test]
    fn 途中なら次の行を待つ() {
        let probe = json!({
            "cmd": {"ok": false, "err": "<input>:1:9: unexpected end of input"},
            "term": {"ok": false, "err": "<input>:1:3: unexpected token"},
        });
        assert_eq!(pure::classify(Some(&probe)), (State::More, Kind::Cmd));
    }

    #[story("C1")]
    #[test]
    fn より先まで解析できたほうをユーザーの意図とみなす() {
        let probe = json!({
            "cmd": {"ok": false, "err": "<input>:1:2: unexpected token"},
            "term": {"ok": false, "err": "<input>:1:7: unexpected token"},
        });
        assert_eq!(pure::classify(Some(&probe)), (State::Err, Kind::Term));
    }

    #[story("C1")]
    #[test]
    fn 判定できなければ_Lean_に本当のエラーを出させる() {
        // leani の推測でエラーを作らない。command として送る。
        assert_eq!(pure::classify(None), (State::Err, Kind::Cmd));
    }

    #[story("E1")]
    #[test]
    fn 証明モードでは_tacticSeq_だけを見る() {
        let ok = json!({"tac": {"ok": true}});
        assert_eq!(pure::classify_tac(Some(&ok)), (State::Complete, Kind::Tac));
        let more = json!({"tac": {"ok": false, "err": "unexpected end of input"}});
        assert_eq!(pure::classify_tac(Some(&more)), (State::More, Kind::Tac));
        assert_eq!(pure::classify_tac(None), (State::Err, Kind::Tac));
    }

    #[story("C1")]
    #[test]
    fn インデントで続いたブロックは空行まで待つ() {
        assert!(pure::block_continues(
            &["def f", "  | 0 => 0"],
            "def f\n  | 0 => 0"
        ));
        assert!(!pure::block_continues(&["def f := 1"], "def f := 1"));
    }
}

mod 評価できない理由 {
    //! #eval のエラー文から、評価できない理由を取り出す。
    use super::*;

    #[story("A2")]
    #[test]
    fn Repr_が無いという理由を取り出す() {
        let blob = "Could not synthesize a `ToExpr`, `Repr`, or `ToString` instance";
        assert!(pure::not_evaluable_reason(blob).is_some_and(|why| why.contains("Repr")));
    }

    #[story("A2")]
    #[test]
    fn noncomputable_という理由を取り出す() {
        let blob = "failed to compile definition, consider marking it as 'noncomputable'";
        assert!(pure::not_evaluable_reason(blob).is_some_and(|why| why.contains("noncomputable")));
    }

    #[story("A2")]
    #[test]
    fn 評価できないエラーでなければ_None_を返す() {
        assert_eq!(pure::not_evaluable_reason("Unknown identifier `x`"), None);
    }
}

mod do_の外の_action {
    use super::*;

    #[story("A1")]
    #[test]
    fn do_の外に書いた_action_のエラーを見分ける() {
        let blob = "Nested action `← f` must be nested inside a `do` expression.";
        assert!(pure::nested_action(blob));
    }

    #[story("A1")]
    #[test]
    fn ほかのエラーは_do_の外の_action_と見なさない() {
        assert!(!pure::nested_action("Unknown identifier `x`"));
    }
}

mod エラー位置の範囲 {
    //! メッセージの位置を、送ったソースの座標からその行の中に収める。
    use super::*;

    #[story("A5")]
    #[test]
    fn eval_で包んだ分だけ位置を戻す() {
        let m = json!({"pos": {"line": 2, "column": 6}, "endPos": {"line": 2, "column": 9}});
        // #eval で 1 行 2 桁ずらして送っているので、元のソースでは 1 行 4 桁。
        assert_eq!(pure::span(&m, &["foo bar"], 1, 2), Some((1, 4, 3)));
    }

    #[story("A5")]
    #[test]
    fn 位置が行の外なら範囲を返さない() {
        let m = json!({"pos": {"line": 9, "column": 0}});
        assert_eq!(pure::span(&m, &["foo"], 0, 0), None);
    }

    #[story("A5")]
    #[test]
    fn 範囲の幅は行の長さに収める() {
        let m = json!({"pos": {"line": 1, "column": 1}, "endPos": {"line": 1, "column": 99}});
        assert_eq!(pure::span(&m, &["abc"], 0, 0), Some((1, 1, 2)));
    }
}

mod レスポンスの切り出し {
    //! 空行で区切られた JSON を、受信し終えた分だけ取り出す。
    use super::*;

    #[story("F1")]
    #[test]
    fn 受信し終えるまでは何も返さない() {
        assert_eq!(pure::first_response("{\"env\": 1}"), None);
    }

    #[story("F1")]
    #[test]
    fn 空行を受け取ったら読み込む() {
        assert_eq!(
            pure::first_response("{\"env\": 1}\n\n"),
            Some(json!({"env": 1}))
        );
    }

    #[story("F1")]
    #[test]
    fn JSON_の中の空行では区切らない() {
        let buf = "{\"messages\": [{\"data\": \"a\\n\\nb\"}]}\n\n";
        let got = pure::first_response(buf).expect("レスポンスを読めない");
        assert_eq!(got["messages"][0]["data"], "a\n\nb");
    }
}

mod エンジンの_PANIC {
    //! エンジンが PANIC したかを、レスポンスのメッセージから判定する。
    use super::*;

    #[story("F1")]
    #[test]
    fn PANIC_の行を返す() {
        let resp = json!({"messages": [
            {"severity": "info", "data": "1"},
            {"severity": "info", "data": PANIC},
        ]});
        assert_eq!(
            pure::panic_line(&resp).as_deref(),
            Some("PANIC at Foo.bar Foo:1:2: oops")
        );
    }

    #[story("F1")]
    #[test]
    fn PANIC_の前に出力があっても_PANIC_の行を返す() {
        let resp = json!({"messages": [{"severity": "info", "data": format!("42\n{PANIC}")}]});
        assert_eq!(
            pure::panic_line(&resp).as_deref(),
            Some("PANIC at Foo.bar Foo:1:2: oops")
        );
    }

    #[story("F1")]
    #[test]
    fn PANIC_していなければ_None_を返す() {
        let resp = json!({"messages": [{"severity": "info", "data": "2"}]});
        assert_eq!(pure::panic_line(&resp), None);
        assert_eq!(pure::panic_line(&json!({})), None);
    }
}

mod 起動前の準備 {
    //! import を追加したり取り除いたりする処理。
    use super::*;

    #[story("G2")]
    #[test]
    fn init_ファイルの_import_行は取り除く() {
        // init は既存の環境の上に追加するので、import を書けない。
        assert_eq!(pure::strip_imports("import Foo\ndef x := 1"), "def x := 1");
    }

    #[story("G2")]
    #[test]
    fn import_を含むファイルには_import_を追加しない() {
        assert!(pure::has_import("\nimport Mathlib\ndef f := 1"));
        assert!(!pure::has_import("def f := 1  -- import ではない"));
    }

    #[story("G1")]
    #[test]
    fn lake_env_のキャッシュを読む() {
        let env = pure::parse_env_lines("LEAN_PATH=/a:/b\nLD_LIBRARY_PATH=\n");
        let want = [("LEAN_PATH", "/a:/b"), ("LD_LIBRARY_PATH", "")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .into();
        assert_eq!(env, want);
    }

    #[story("G4", "G6")]
    #[test]
    fn toolchain_が変わったらキャッシュを作り直す() {
        // LEAN_PATH は core の .olean も指す。前のバージョンのキャッシュを再利用すると、
        // repl は起動するのに import がすべて失敗して、原因が分からなくなる。
        let dir = tmp();
        let project = dir.path().join("proj");
        fs::create_dir(&project).expect("mkdir");
        let manifest = project.join("lake-manifest.json");
        fs::write(&manifest, "{}").expect("write");
        let cache = dir.path().join("cache");
        fs::write(&cache, "LEAN_PATH=old\n").expect("write");

        touch(&manifest, 1);
        touch(&cache, 2);
        assert!(!boot::lake_env_stale(
            &path_str(&cache),
            &path_str(&project)
        ));

        let tc = project.join("lean-toolchain");
        fs::write(&tc, "leanprover/lean4:v4.33.1\n").expect("write");
        touch(&tc, 3);
        assert!(boot::lake_env_stale(&path_str(&cache), &path_str(&project)));
    }
}

mod エンジンのバージョン {
    //! 使う Lean のバージョンから repl のタグとパスを決める。
    use super::*;

    const TAGS: [&str; 3] = ["v4.32.0", "v4.33.0", "v4.34.0-rc2"];

    #[story("G3")]
    #[test]
    fn toolchain_からバージョンだけ取る() {
        assert_eq!(
            pure::toolchain_version("leanprover/lean4:v4.34.0-rc2"),
            "v4.34.0-rc2"
        );
        assert_eq!(pure::toolchain_version("v4.33.0"), "v4.33.0");
    }

    #[story("G4")]
    #[test]
    fn 自分でビルドしたエンジンの_toolchain_が違えば警告の文を返す() {
        let got = pure::toolchain_warning(true, "v4.33.0", "v4.34.0", "/eng").expect("警告しない");
        assert!(
            got.contains("使うバージョン=v4.34.0 / エンジン=v4.33.0"),
            "{got}"
        );
        assert!(got.contains("cd /eng && lake build repl"), "{got}");
    }

    #[story("G4")]
    #[test]
    fn toolchain_が同じか_leani_が用意したエンジンなら警告しない() {
        assert_eq!(
            pure::toolchain_warning(true, "v4.34.0", "v4.34.0", "/e"),
            None
        );
        assert_eq!(
            pure::toolchain_warning(false, "v4.33.0", "v4.34.0", "/e"),
            None
        );
        // lean-toolchain が読めなかったときは比べない。
        assert_eq!(pure::toolchain_warning(true, "", "v4.34.0", "/e"), None);
    }

    #[story("G3")]
    #[test]
    fn rc_は同じバージョンの正式リリースより前() {
        assert!(pure::version_key("v4.33.0-rc1") < pure::version_key("v4.33.0"));
        assert!(pure::version_key("v4.33.0") < pure::version_key("v4.34.0-rc1"));
    }

    #[story("G3")]
    #[test]
    fn 解釈できないバージョンは_None() {
        for tag in ["v4.33", "nightly-2026-01-01", "4.33.0"] {
            assert_eq!(pure::version_key(tag), None, "解釈できてしまう: {tag:?}");
        }
    }

    #[story("G3")]
    #[test]
    fn 同名のタグがあればそれを使う() {
        assert_eq!(pure::pick_tag("v4.33.0", &TAGS).as_deref(), Some("v4.33.0"));
    }

    #[story("G3")]
    #[test]
    fn タグの無い_patch_リリースは直前のタグを使う() {
        // repl は v4.33.1 にタグを付けない。API は patch で変わらない。
        assert_eq!(pure::pick_tag("v4.33.1", &TAGS).as_deref(), Some("v4.33.0"));
    }

    #[story("G3")]
    #[test]
    fn どのタグより古いバージョンなら_None() {
        assert_eq!(pure::pick_tag("v4.0.0", &["v4.32.0", "v4.33.0"]), None);
        assert_eq!(pure::pick_tag("nightly", &["v4.33.0"]), None);
    }

    #[story("G3")]
    #[test]
    fn パスはバージョンごとに分かれる() {
        let got = pure::engine_dir(None, "leanprover/lean4:v4.33.0", "/cache");
        assert_eq!(got.as_deref(), Ok("/cache/v4.33.0"));
    }

    #[story("G3")]
    #[test]
    fn 明示されたパスはそのまま使う() {
        let got = pure::engine_dir(Some("/opt/repl"), "leanprover/lean4:v4.33.0", "/cache");
        assert_eq!(got.as_deref(), Ok("/opt/repl"));
    }

    #[story("G3")]
    #[test]
    fn パスの外を指すバージョン名は受け付けない() {
        // build_engine はこの場所を消してから作り直す。`..` を含む名前を
        // 受け付けると、関係ないディレクトリを消してしまう。
        for tc in ["leanprover/lean4:..", "..", "."] {
            assert!(pure::engine_dir(None, tc, "/cache").is_err(), "{tc:?}");
        }
    }
}

// ----------------------------------------------- 設定のテストのデータ

/// HOME が /home で、vars の環境変数だけがある Ctx。
fn ctx_with(vars: &[(&str, &str)], cwd: &str) -> Ctx {
    let var = |k: &str| match (k, vars.iter().find(|(name, _)| *name == k)) {
        (_, Some((_, v))) => Some(v.to_string()),
        ("HOME", None) => Some("/home".to_string()),
        _ => None,
    };
    Ctx::new(var, cwd, false)
}

fn ctx_at(cwd: &str) -> Ctx {
    ctx_with(&[], cwd)
}

fn table(src: &str) -> toml::Table {
    src.parse().expect("TOML として読めない")
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn toolchain_dir(dir: &Path, name: &str, tc: &str) -> String {
    let d = dir.join(name);
    fs::create_dir(&d).expect("mkdir");
    fs::write(d.join("lean-toolchain"), format!("{tc}\n")).expect("write");
    path_str(&d)
}

fn built_engine(dir: &Path, tc: &str) -> String {
    let eng = toolchain_dir(dir, "eng", tc);
    fs::create_dir_all(format!("{eng}/.lake/build/bin")).expect("mkdir");
    fs::write(format!("{eng}/.lake/build/bin/repl"), "").expect("write");
    eng
}

/// names の実行ファイルだけを置いたディレクトリを、PATH として返す。
/// テストは実行ファイルを実行しないので、中身は空のスクリプトにする。
fn tools_path(dir: &Path, names: &[&str]) -> String {
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).expect("mkdir");
    for name in names {
        let tool = bin.join(name);
        fs::write(&tool, "#!/bin/sh\n").expect("write");
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    path_str(&bin)
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// ファイルの更新時刻を、1970 年から secs 秒の時刻にする。
fn touch(path: &Path, secs: u64) {
    fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(secs)))
        .expect("set_modified");
}

/// 名前と中身を決めたシェルスクリプトを dir/bin に置き、PATH として返す。
///
/// boot は子プロセスに Ctx の PATH しか渡さないので、スクリプトが mkdir や sleep を
/// 使えるように、スクリプトの先頭で PATH を標準のものにする。
fn fake_tools(dir: &Path, tools: &[(&str, &str)]) -> String {
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).expect("mkdir");
    for (name, body) in tools {
        let tool = bin.join(name);
        fs::write(&tool, format!("#!/bin/sh\nPATH=/bin:/usr/bin\n{body}\n")).expect("write");
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    path_str(&bin)
}

/// 表示を受け取るだけの出口。boot の表示はテストでは確かめない。
fn sink() -> Vec<u8> {
    Vec::new()
}

mod 設定の値の型 {
    //! TOML には何でも書ける。型が違う値は panic ではなく ConfigError にする。
    use super::*;

    fn resolve_err(src: &str) -> String {
        config::resolve(&ctx_at("/"), None, None, &[], &table(src))
            .expect_err("エラーにならない")
            .0
    }

    #[story("G4")]
    #[test]
    fn import_を文字列で書いたらエラーにする() {
        // エラーにせず受け付けると 1 文字ずつの import になり、どれも解決できないので
        // ヘッダがすべて捨てられる (import Lean も消える)。
        let why = resolve_err("default = \"m\"\n[env.m]\nimports = \"Mathlib\"\n");
        assert!(why.contains("配列"), "{why}");
    }

    #[story("G4")]
    #[test]
    fn 文字列で書くべき値が別の型ならエラーにする() {
        for (key, value) in [("project", "3"), ("engine", "true"), ("prompt", "5")] {
            let why = resolve_err(&format!("default = \"m\"\n[env.m]\n{key} = {value}\n"));
            assert!(why.contains(key), "{why}");
        }
    }

    #[story("G4")]
    #[test]
    fn 表で書くべき値が別の型ならエラーにする() {
        let why = resolve_err("default = \"m\"\nenv = 3\n");
        assert!(why.contains("env は表"), "{why}");
        let why = resolve_err("default = \"m\"\n[env]\nm = \"nope\"\n");
        assert!(why.contains("env.m は表"), "{why}");
    }

    #[story("G4")]
    #[test]
    fn default_が文字列でなければエラーにする() {
        let why = resolve_err("default = 3\n[env.m]\n");
        assert!(why.contains("default"), "{why}");
    }

    #[story("G4", "G5")]
    #[test]
    fn 書き方が正しければ値をそのまま読み込む() {
        let cfg = table("default = \"m\"\n[env.m]\nimports = [\"A\", \"B\"]\nprompt = \"> \"\n");
        let got = config::resolve(&ctx_at("/"), None, None, &[], &cfg).expect("resolve");
        assert_eq!(
            (got.name.as_str(), got.imports, got.prompt.as_str()),
            ("m", vec!["A".to_string(), "B".to_string()], "> ")
        );
    }
}

mod 起動する環境の決め方 {
    //! guide/config.md の「起動する環境の決まり方」と、エンジンの優先順位。
    use super::*;

    const CFG: &str = r#"
default = "d"
engine = "/top/engine"

[env.d]
imports = ["D"]

[env.m]
project = "/m"
imports = ["M"]
engine = "/m/engine"
"#;

    fn resolve(
        ctx: &Ctx,
        name: Option<&str>,
        project: Option<&str>,
        imports: &[&str],
    ) -> EnvConfig {
        let imports = imports.iter().map(|m| m.to_string()).collect::<Vec<_>>();
        config::resolve(ctx, name, project, &imports, &table(CFG)).expect("resolve")
    }

    #[story("G5")]
    #[test]
    fn 名前を指定したら_default_より優先する() {
        assert_eq!(resolve(&ctx_at("/"), Some("m"), None, &[]).name, "m");
    }

    #[story("G5")]
    #[test]
    fn 名前と_import_を指定したら環境の_imports_を置き換える() {
        let got = resolve(&ctx_at("/"), Some("m"), Some("/p"), &["X"]);
        assert_eq!(
            (got.name.as_str(), got.imports, got.project.as_deref()),
            ("m", vec!["X".to_string()], Some("/p"))
        );
    }

    #[story("G5", "G6")]
    #[test]
    fn import_だけを指定したら_default_を使わない() {
        let dir = tmp();
        let got = resolve(&ctx_at(&path_str(dir.path())), None, None, &["X"]);
        assert_eq!(
            (got.name.as_str(), got.imports),
            ("plain", vec!["X".to_string()])
        );
    }

    #[story("G3", "G5")]
    #[test]
    fn 環境の_engine_は最上位の_engine_より優先する() {
        let ctx = ctx_at("/");
        let engine = |name| resolve(&ctx, name, None, &[]).engine;
        assert_eq!(engine(Some("m")).as_deref(), Some("/m/engine"));
        assert_eq!(engine(None).as_deref(), Some("/top/engine"));
    }

    #[story("G3")]
    #[test]
    fn 設定に_engine_が無ければ_LEANI_ENGINE_を使う() {
        let ctx = ctx_with(&[("LEANI_ENGINE", "/env/engine")], "/");
        let cfg = table("default = \"d\"\n[env.d]\n");
        let got = config::resolve(&ctx, None, None, &[], &cfg).expect("resolve");
        assert_eq!(got.engine.as_deref(), Some("/env/engine"));
    }

    #[story("G3", "G5")]
    #[test]
    fn 名前を付けずに起動したら環境の_engine_は使わない() {
        let dir = tmp();
        let got = resolve(&ctx_at(&path_str(dir.path())), None, None, &["X"]);
        assert_eq!(got.engine.as_deref(), Some("/top/engine"));
    }
}

mod 起動を中止する理由 {
    use super::*;

    /// elan、lake、git がそろった PATH の Ctx。
    fn ctx(dir: &Path) -> Ctx {
        ctx_with(&[("PATH", &tools_path(dir, &["elan", "lake", "git"]))], "/")
    }

    fn make(ctx: &Ctx, project: Option<&str>, engine: Option<&str>) -> EnvConfig {
        EnvConfig::make(ctx, "t", project, &[], None, engine)
    }

    #[story("G3", "G4")]
    #[test]
    fn エンジンとプロジェクトのバージョンが違えば手順を示して起動を中止する() {
        let dir = tmp();
        let eng = built_engine(dir.path(), "leanprover/lean4:v4.33.0");
        let proj = toolchain_dir(dir.path(), "proj", "leanprover/lean4:v4.34.0");
        let ctx = ctx(dir.path());
        let why = config::problem(&ctx, &make(&ctx, Some(&proj), Some(&eng))).unwrap_or_default();
        assert!(why.contains("バージョンがプロジェクトと違う"), "{why}");
        assert!(why.contains("lake build repl"), "{why}");
    }

    #[story("G4")]
    #[test]
    fn Lake_プロジェクトが無ければ起動を中止する() {
        let dir = tmp();
        let eng = built_engine(dir.path(), "leanprover/lean4:v4.33.0");
        let no = path_str(&dir.path().join("no"));
        let ctx = ctx(dir.path());
        let why = config::problem(&ctx, &make(&ctx, Some(&no), Some(&eng))).unwrap_or_default();
        assert!(why.contains("Lake プロジェクトが無い"), "{why}");
    }

    #[story("G3", "G4")]
    #[test]
    fn 指定されたエンジンが無いか未ビルドなら起動を中止する() {
        let dir = tmp();
        let ctx = ctx(dir.path());
        let no = path_str(&dir.path().join("no"));
        let why = config::problem(&ctx, &make(&ctx, None, Some(&no))).unwrap_or_default();
        assert!(why.contains("エンジンが無い"), "{why}");
        let eng = toolchain_dir(dir.path(), "eng", "leanprover/lean4:v4.33.0");
        let why = config::problem(&ctx, &make(&ctx, None, Some(&eng))).unwrap_or_default();
        assert!(why.contains("未ビルド"), "{why}");
    }

    #[story("G4")]
    #[test]
    fn elan_lake_git_が_PATH_に無ければ起動を中止する() {
        // git はエンジンを leani が用意するときだけ必要になる。
        for tool in ["elan", "lake", "git"] {
            let dir = tmp();
            let others = ["elan", "lake", "git"]
                .into_iter()
                .filter(|t| *t != tool)
                .collect::<Vec<_>>();
            let ctx = ctx_with(&[("PATH", &tools_path(dir.path(), &others))], "/");
            let why = config::problem(&ctx, &make(&ctx, None, None)).unwrap_or_default();
            assert!(why.contains(&format!("{tool} が PATH に無い")), "{why}");
        }
    }

    #[story("G4")]
    #[test]
    fn 設定がそろっていれば中止しない() {
        let dir = tmp();
        let eng = built_engine(dir.path(), "leanprover/lean4:v4.33.0");
        let proj = toolchain_dir(dir.path(), "proj", "leanprover/lean4:v4.33.0");
        let ctx = ctx(dir.path());
        assert_eq!(
            config::problem(&ctx, &make(&ctx, Some(&proj), Some(&eng))),
            None
        );
    }
}

mod 設定ファイルの読み込み {
    use super::*;

    #[story("G4")]
    #[test]
    fn TOML_として読めない設定ファイルは_ConfigError_にする() {
        let dir = tmp();
        let path = dir.path().join("config.toml");
        fs::write(&path, "default = \n").expect("write");
        let why = config::load_config(&path_str(&path)).expect_err("エラーにならない");
        assert!(why.0.contains("設定ファイルを読めない"), "{why}");
    }

    #[story("G4", "G6")]
    #[test]
    fn 設定ファイルが無ければ空の設定として読む() {
        let dir = tmp();
        let got = config::load_config(&path_str(&dir.path().join("none.toml")));
        assert_eq!(got, Ok(toml::Table::new()));
    }
}

mod 使うバージョンの決め方 {
    //! elan run に渡すバージョン。
    //!
    //! エンジンをビルドしたバージョンと合わないと .olean を読み込めない。
    use super::*;

    #[story("G3")]
    #[test]
    fn プロジェクトのバージョンが最優先() {
        let dir = tmp();
        let ctx = ctx_at("/");
        let proj = toolchain_dir(dir.path(), "proj", "leanprover/lean4:v4.34.0-rc2");
        let eng = toolchain_dir(dir.path(), "eng", "leanprover/lean4:v4.33.0");
        let cfg = EnvConfig::make(&ctx, "t", Some(&proj), &[], None, Some(&eng));
        assert_eq!(
            boot::guess_toolchain(&ctx, &cfg).as_deref(),
            Some("leanprover/lean4:v4.34.0-rc2")
        );
    }

    #[story("G3")]
    #[test]
    fn 明示したエンジンのバージョンを使う() {
        // 自分でビルドしたエンジンは leani がビルドし直さないので、そのバージョンに
        // 合わせるしかない。
        // elan のデフォルトのバージョンを使うと、4.34 の .olean を 4.33 で
        // 読み込むことになる。
        let dir = tmp();
        let ctx = ctx_at("/");
        let eng = toolchain_dir(dir.path(), "eng", "leanprover/lean4:v4.33.0");
        let cfg = EnvConfig::make(&ctx, "t", None, &[], None, Some(&eng));
        assert_eq!(
            boot::guess_toolchain(&ctx, &cfg).as_deref(),
            Some("leanprover/lean4:v4.33.0")
        );
    }

    #[story("G3")]
    #[test]
    fn どちらも無ければ_elan_のデフォルト() {
        let dir = tmp();
        let lean = "echo 'Lean (version 4.9.0, x86_64-apple-darwin, commit abc, Release)'";
        let path = fake_tools(dir.path(), &[("lean", lean)]);
        let ctx = ctx_with(&[("PATH", &path)], "/");
        let cfg = EnvConfig::make(&ctx, "t", None, &[], None, None);
        assert_eq!(
            boot::guess_toolchain(&ctx, &cfg).as_deref(),
            Some("leanprover/lean4:v4.9.0")
        );
    }
}

mod 手動でエンジンを用意する手順 {
    //! leani が自動で用意できなかったときに表示する。書いてあるとおりに実行すると、
    //! 自動で用意したものと同じものができる。
    use super::*;

    #[story("G3")]
    #[test]
    fn タグとバージョンの書き換えまで含む() {
        let out = boot::manual_setup("/x/e", "leanprover/lean4:v4.33.1", Some("v4.33.0"));
        assert!(out.contains("--branch v4.33.0"), "{out}");
        assert!(
            out.contains("leanprover/lean4:v4.33.1 > /x/e/lean-toolchain"),
            "{out}"
        );
    }

    #[story("G3")]
    #[test]
    fn タグが分からなければ選び方を書く() {
        let out = boot::manual_setup("/x/e", "leanprover/lean4:v4.33.1", None);
        assert!(out.contains("v4.33.1 以下で一番新しいタグ"), "{out}");
    }
}

// ----------------------------------------------- エンジンを用意するテストのデータ

const TC: &str = "leanprover/lean4:v4.33.0";

/// clone と build の代わりをする git と lake。
///
/// git は呼ばれた引数を log に追記し、`ls-remote` なら tags を表示し、`clone` なら
/// clone 先のディレクトリを作る。lake は `build` なら repl を作り、`env` なら
/// LEAN_PATH を表示して count に 1 行追記する。
fn fake_git(log: &Path, tags: &[&str]) -> String {
    let lines = tags
        .iter()
        .map(|t| format!("printf 'a1\\trefs/tags/{t}\\n'"))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "echo \"$*\" >> {}\ncase \"$1\" in\n  ls-remote) {lines} ;;\n  clone) for a; do d=$a; done; mkdir -p \"$d\" ;;\nesac",
        log.display()
    )
}

fn fake_lake(count: &Path) -> String {
    format!(
        "case \"$1\" in\n  build) mkdir -p .lake/build/bin && : > .lake/build/bin/repl ;;\n  env) echo run >> {}; printf 'LEAN_PATH=/a\\n' ;;\nesac",
        count.display()
    )
}

/// git と lake の偽物を置き、エンジンの置き場所を dir の下にした Ctx。
fn setup_ctx(dir: &Path, tags: &[&str], vars: &[(&str, &str)]) -> Ctx {
    let path = fake_tools(
        dir,
        &[
            ("git", &fake_git(&dir.join("git.log"), tags)),
            ("lake", &fake_lake(&dir.join("lake.count"))),
        ],
    );
    let state = path_str(&dir.join("state"));
    let mut all = vec![("PATH", path.as_str()), ("XDG_STATE_HOME", state.as_str())];
    all.extend_from_slice(vars);
    ctx_with(&all, "/")
}

fn git_log(dir: &Path) -> String {
    fs::read_to_string(dir.join("git.log")).unwrap_or_default()
}

mod エンジンを用意するときの安全対策 {
    //! leani が削除してよいのは、leani 自身が作ったディレクトリだけ。
    //! leani を 2 つ同時に実行してもよい。
    use super::*;

    #[story("G3")]
    #[test]
    fn 明示されたエンジンは作り直さない() {
        // build_engine は置き場所を作り直すので、ユーザーが指定した場所に対して
        // 実行してはならない。git も lake も無い PATH なので、作ろうとすれば失敗する。
        let dir = tmp();
        let ctx = ctx_with(&[("PATH", &path_str(dir.path()))], "/");
        let eng = toolchain_dir(dir.path(), "eng", TC);
        let got = boot::ensure_engine(&ctx, &mut sink(), Some(&eng), TC, false);
        let why = got.expect_err("用意してしまった").0;
        assert!(why.contains("未ビルド"), "{why}");
        assert!(Path::new(&eng).join("lean-toolchain").is_file());
    }

    #[story("G3", "G4")]
    #[test]
    fn 明示されたエンジンにバージョンが無ければ起動を中止する() {
        // 合わせられるバージョンはこのエンジンのものしかない。何も表示せずに elan の
        // デフォルトを使うと .olean を読み込めない。
        let dir = tmp();
        let ctx = ctx_with(
            &[("PATH", &tools_path(dir.path(), &["elan", "lake", "git"]))],
            "/",
        );
        let eng = dir.path().join("eng");
        fs::create_dir_all(eng.join(".lake/build/bin")).expect("mkdir");
        fs::write(eng.join(".lake/build/bin/repl"), "").expect("write");
        let cfg = EnvConfig::make(&ctx, "t", None, &[], None, Some(&path_str(&eng)));
        let why = config::problem(&ctx, &cfg).unwrap_or_default();
        assert!(why.contains("lean-toolchain"), "{why}");
    }

    #[story("G3")]
    #[test]
    fn 作業中の置き場所はプロセスごとに分かれる() {
        // leani を 2 つ同時に起動したとき、同じ作業用のディレクトリを使うと片方が
        // 起動できない。置き場所とは別の名前で作業して、終わったら消す。
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &[], &[]);
        let path = path_str(&dir.path().join("engine/v4.33.0"));
        boot::build_engine(&ctx, &mut sink(), &path, TC, "v4.33.0").expect("build");

        let log = git_log(dir.path());
        let work = log.trim().rsplit(' ').next().unwrap_or_default();
        assert!(
            work.starts_with(&format!("{path}.")) && work.ends_with(".tmp"),
            "{log}"
        );
        assert!(Path::new(&path).join(".lake/build/bin/repl").is_file());
        assert!(!Path::new(work).exists(), "作業中の置き場所が残っている");
    }

    #[story("G3")]
    #[test]
    fn 待っているあいだに別の_leani_が置いたエンジンは消さない() {
        // 消すと、そのエンジンを使っている別の leani がエンジンを起動できなくなる。
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &[], &[]);
        let path = dir.path().join("engine/v4.33.0");
        fs::create_dir_all(path.join(".lake/build/bin")).expect("mkdir");
        fs::write(path.join(".lake/build/bin/repl"), "先にあったもの").expect("write");

        boot::build_engine(&ctx, &mut sink(), &path_str(&path), TC, "v4.33.0").expect("build");

        let repl = fs::read_to_string(path.join(".lake/build/bin/repl")).expect("read");
        assert_eq!(repl, "先にあったもの");
        let log = git_log(dir.path());
        let work = log.trim().rsplit(' ').next().unwrap_or_default();
        assert!(!Path::new(work).exists(), "作業中の置き場所が残っている");
    }
}

mod エンジンを自動で用意できないとき {
    //! 初回の起動で通る処理。失敗しても、手動で用意する手順を表示する。
    use super::*;

    #[story("G3")]
    #[test]
    fn タグを取得できなければ手動の手順を表示する() {
        // 終わらない、git が無い、失敗する、の 3 つ。exec で sh ごと sleep にするのは、
        // kill したあとに sleep だけが残ってパイプを閉じないため。
        let dir = tmp();
        let fails = [Some("exec sleep 5"), None, Some("echo fatal >&2; exit 1")];
        for fail in fails {
            let git = fail.map(|body| ("git", body));
            let path = fake_tools(dir.path(), &git.into_iter().collect::<Vec<_>>());
            let mut ctx = ctx_with(&[("PATH", &path)], "/");
            ctx.setup_timeout = Duration::from_millis(200);
            let got = boot::ensure_engine(&ctx, &mut sink(), None, TC, false);
            let why = got.expect_err("用意できてしまった").0;
            assert!(why.contains("手動で用意する場合"), "{why}");
            assert!(why.contains("git clone --branch"), "{why}");
            let _ = fs::remove_file(dir.path().join("bin/git"));
        }
    }

    #[story("G3")]
    #[test]
    fn タグの一覧から_refs_tags_の名前だけを取り出す() {
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &["v4.33.0", "v4.34.0-rc1"], &[]);
        assert_eq!(
            boot::fetch_tags(&ctx).expect("fetch"),
            ["v4.33.0", "v4.34.0-rc1"]
        );
    }

    #[story("G3")]
    #[test]
    fn バージョンとして解釈できなければ_master_で試す() {
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &["v4.33.0"], &[]);
        boot::ensure_engine(&ctx, &mut sink(), None, "leanprover/lean4:nightly", false)
            .expect("ensure");
        let log = git_log(dir.path());
        assert!(log.contains("--branch master"), "{log}");
    }

    #[story("G3", "G4")]
    #[test]
    fn 対応するタグが無ければ起動を中止する() {
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &["v4.40.0"], &[]);
        let got = boot::ensure_engine(&ctx, &mut sink(), None, TC, false);
        let why = got.expect_err("用意できてしまった").0;
        assert!(why.contains("repl に v4.33.0 用のタグが無い"), "{why}");
        assert!(why.contains("以下で一番新しいタグ"), "{why}");
        assert!(!git_log(dir.path()).contains("clone"), "ビルドしてしまった");
    }

    #[story("G3")]
    #[test]
    fn LEANI_NO_SETUP_のときはエンジンを用意しない() {
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &["v4.33.0"], &[("LEANI_NO_SETUP", "1")]);
        let got = boot::ensure_engine(&ctx, &mut sink(), None, TC, false);
        let why = got.expect_err("用意できてしまった").0;
        assert!(why.contains("LEANI_NO_SETUP"), "{why}");
        assert_eq!(git_log(dir.path()), "", "git を呼んでしまった");

        // --setup で明示的に頼まれたときは用意する。
        let path = boot::ensure_engine(&ctx, &mut sink(), None, TC, true).expect("ensure");
        assert!(Path::new(&path).join(".lake/build/bin/repl").is_file());
    }

    #[story("G3")]
    #[test]
    fn ビルドに失敗したら手動の手順にタグを含める() {
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &["v4.32.0"], &[]);
        let git = fake_git(&dir.path().join("git.log"), &["v4.32.0"]);
        fake_tools(dir.path(), &[("git", &git), ("lake", "exit 1")]);
        let got = boot::ensure_engine(&ctx, &mut sink(), None, TC, false);
        let why = got.expect_err("用意できてしまった").0;
        assert!(why.contains("--branch v4.32.0"), "{why}");
    }
}

mod lake_env_の取得 {
    //! lake env の結果はキャッシュして、起動のたびに実行しない。
    use super::*;

    fn project(dir: &Path) -> String {
        let proj = dir.join("proj");
        fs::create_dir(&proj).expect("mkdir");
        path_str(&proj)
    }

    fn lake_count(dir: &Path) -> usize {
        fs::read_to_string(dir.join("lake.count"))
            .unwrap_or_default()
            .lines()
            .count()
    }

    #[story("G1")]
    #[test]
    fn プロジェクトが無ければ_lake_env_を実行しない() {
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &[], &[]);
        assert_eq!(boot::lake_env(&ctx, None, TC), Ok(Default::default()));
        assert_eq!(lake_count(dir.path()), 0);
    }

    #[story("G1", "G6")]
    #[test]
    fn キャッシュが新しければ_lake_env_を実行しない() {
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &[], &[]);
        let proj = project(dir.path());
        let want: std::collections::BTreeMap<_, _> =
            [("LEAN_PATH".to_string(), "/a".to_string())].into();
        assert_eq!(boot::lake_env(&ctx, Some(&proj), TC), Ok(want.clone()));
        assert_eq!(boot::lake_env(&ctx, Some(&proj), TC), Ok(want));
        assert_eq!(lake_count(dir.path()), 1);
    }

    #[story("G4", "G6")]
    #[test]
    fn lake_env_を実行できなければ理由を知らせる() {
        let dir = tmp();
        let proj = project(dir.path());
        let fails = [
            (Some("exec sleep 5"), "秒で終わらなかった"),
            (None, "実行できなかった"),
            (Some("echo boom >&2; exit 1"), "失敗した"),
        ];
        for (fail, why) in fails {
            let lake = fail.map(|body| ("lake", body));
            let path = fake_tools(dir.path(), &lake.into_iter().collect::<Vec<_>>());
            let state = path_str(&dir.path().join("state"));
            let mut ctx = ctx_with(&[("PATH", &path), ("XDG_STATE_HOME", &state)], "/");
            // 200 ms にすると、テストを並列で実行したときに、失敗する偽の lake も
            // 時間内に終わらないことがある。
            ctx.setup_timeout = Duration::from_secs(1);
            let got = boot::lake_env(&ctx, Some(&proj), TC)
                .expect_err("取得できてしまった")
                .0;
            assert!(got.starts_with("lake env "), "{got}");
            assert!(got.contains(why), "{got}");
            let _ = fs::remove_file(dir.path().join("bin/lake"));
        }
    }

    #[story("G6")]
    #[test]
    fn lake_env_の結果を保存できなければ知らせる() {
        // キャッシュの置き場所にディレクトリがあると rename できない。
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &[], &[]);
        let proj = project(dir.path());
        let key = sha1_smol::Sha1::from(format!("{proj}\n{TC}"))
            .digest()
            .to_string();
        let cache = format!("{}/lake-env/proj-{}", ctx.state, &key[..12]);
        fs::create_dir_all(&cache).expect("mkdir");

        let got = boot::lake_env(&ctx, Some(&proj), TC)
            .expect_err("取得できてしまった")
            .0;
        assert!(got.contains("保存できなかった"), "{got}");
        let left = fs::read_dir(format!("{}/lake-env", ctx.state))
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(left, 0, "一時ファイルが残っている");
    }
}

const POS_OUT: &str = r#"{"pos": {"line": 99, "column": 0}, "endPos": {"line": 99, "column": 5}}"#;

/// src の中の nth 番目 (0 始まり) の word を、repl と同じ {line, column} で指す。
/// 桁はコードポイント単位。
fn mark(src: &str, word: &str, nth: usize) -> Json {
    let at = src
        .match_indices(word)
        .nth(nth)
        .map(|(i, _)| i)
        .expect("word が無い");
    let line = src[..at].matches('\n').count() + 1;
    let col = src[..at].rsplit('\n').next().unwrap_or("").chars().count();
    let len = word.chars().count();
    json!({
        "pos": {"line": line, "column": col},
        "endPos": {"line": line, "column": col + len},
    })
}

mod 証明モードのタクティクの記録 {
    use super::*;

    fn start() -> Proof {
        Proof::new(0, vec!["⊢ p ∧ q".to_string()], 1)
    }

    fn strings(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }

    #[story("E1")]
    #[test]
    fn タクティクを進めると前の状態とゴールを積む() {
        let got = pure::tactic_step(&start(), 3, &strings(&["⊢ p", "⊢ q"]), "constructor");
        assert_eq!(
            (got.state, &got.goals, got.generation),
            (3, &strings(&["⊢ p", "⊢ q"]), 1)
        );
        assert_eq!(got.script, strings(&["constructor"]));
        assert_eq!(got.stack, [(0, strings(&["⊢ p ∧ q"]))]);
    }

    #[story("E1")]
    #[test]
    fn 取り消すと状態とゴールとスクリプトがそろって戻る() {
        let step = pure::tactic_step(&start(), 3, &strings(&["⊢ p", "⊢ q"]), "constructor");
        assert_eq!(pure::tactic_undo(&step), Some(start()));
        assert_eq!(pure::tactic_undo(&start()), None);
    }
}

mod sorry_の置き換え {
    //! repl の pos / endPos で切り出してスクリプトに置き換える。桁はコードポイント
    //! 単位で、`pure::offset` が byte の位置に直す。
    use super::*;

    fn splice(src: &str, word: &str, script: &str) -> Option<String> {
        pure::splice_sorry(src, &mark(src, word, 0), script)
    }

    #[story("E3")]
    #[test]
    fn 日本語を含んでも位置で切り出せる() {
        let src = "theorem a : True := by /- あいう -/ sorry";
        assert_eq!(
            splice(src, "sorry", "trivial").as_deref(),
            Some("theorem a : True := by /- あいう -/ trivial")
        );
    }

    #[story("E3")]
    #[test]
    fn 単独の行にある_sorry_のインデントを変えない() {
        // 空白を詰めるとインデントが浅くなり、内側の by ブロックの外に出てしまう。
        let src = "example : True := by\n  have h : True := by\n    sorry\n  exact h";
        assert_eq!(
            splice(src, "sorry", "trivial").as_deref(),
            Some("example : True := by\n  have h : True := by\n    trivial\n  exact h")
        );
    }

    #[story("E3")]
    #[test]
    fn 複数行のスクリプトは_sorry_の桁にそろえる() {
        let src = "example : True := by\n  have h : True := by\n    sorry\n  exact h";
        assert_eq!(
            splice(src, "sorry", "constructor\n-- おわり").as_deref(),
            Some(concat!(
                "example : True := by\n",
                "  have h : True := by\n",
                "    constructor\n",
                "    -- おわり\n",
                "  exact h",
            ))
        );
    }

    #[story("E3")]
    #[test]
    fn 行の途中の_sorry_を複数行のスクリプトにするときは_by_の次の行に下げる() {
        let src = "theorem t (n : Nat) : n + 0 = n := by sorry";
        assert_eq!(
            splice(src, "sorry", "induction n with\n| zero => rfl").as_deref(),
            Some("theorem t (n : Nat) : n + 0 = n := by\n  induction n with\n  | zero => rfl")
        );
    }

    #[story("E3")]
    #[test]
    fn 匿名コンストラクタの中の_sorry_に空白を足さない() {
        let src = "example : True ∧ True := ⟨by sorry, by trivial⟩";
        assert_eq!(
            splice(src, "sorry", "trivial").as_deref(),
            Some("example : True ∧ True := ⟨by trivial, by trivial⟩")
        );
    }

    #[story("E3")]
    #[test]
    fn 位置が_sorry_を指していなければ置き換えない() {
        let src = "example : True := by trivial";
        // 位置は合っているが指しているのは sorry ではない / 行が無い。
        assert_eq!(splice(src, "trivial", "rfl"), None);
        let out: Json = serde_json::from_str(POS_OUT).expect("POS_OUT");
        assert_eq!(pure::splice_sorry(src, &out, "rfl"), None);
        let none = json!({"pos": null, "endPos": null});
        assert_eq!(pure::splice_sorry(src, &none, "trivial"), None);
    }

    #[story("E3")]
    #[proptest]
    fn 行と桁を文字のインデックスに変換できる(
        #[strategy(prop::collection::vec("[ab \tあ]{0,4}", 1..=5))] lines: Vec<String>,
        #[strategy(0..100usize)] n: usize,
        #[strategy(0..100usize)] col: usize,
    ) {
        // splice_sorry が切る位置はこれで決まる。1 桁ずれると sorry の途中で
        // 切ってしまう。桁は文字の数で、返すのは byte の位置。
        let src = lines.join("\n");
        let n = n % lines.len();
        let col = col % (lines[n].chars().count() + 1);
        let i = pure::offset(&src, Some(&json!({"line": n + 1, "column": col})));
        prop_assert!(i.is_some());
        let rest = src[i.unwrap_or(0)..].split('\n').next().unwrap_or("");
        let want: String = lines[n].chars().skip(col).collect();
        prop_assert_eq!(rest, want);
    }

    #[story("E3")]
    #[proptest]
    fn sorry_の前後は書き換えない(
        #[strategy(decoys())] before: Vec<&'static str>,
        #[strategy(prop::sample::select(vec!["", "  ", "    ", "  exact ⟨"]))] pad: &'static str,
        #[strategy(decoys())] after: Vec<&'static str>,
        #[strategy(prop::sample::select(vec!["rfl", "induction n with\n| zero => rfl"]))]
        script: &'static str,
    ) {
        // 前後にコメントとして "sorry" を混ぜる。"sorry" の出現を数えて位置を
        // 決めていると、そちらを切ってしまう。位置で切り出している限り、前後は
        // 1 文字も変わらない。
        let line = format!("{pad}sorry");
        let src = [before.as_slice(), &[line.as_str()], after.as_slice()]
            .concat()
            .join("\n");
        let row = before.len() + 1;
        let col = pad.chars().count();
        let sy = json!({
            "pos": {"line": row, "column": col},
            "endPos": {"line": row, "column": col + "sorry".len()},
        });
        let a = pure::offset(&src, sy.get("pos")).unwrap_or(0);
        let b = pure::offset(&src, sy.get("endPos")).unwrap_or(0);
        let got = pure::splice_sorry(&src, &sy, script);

        prop_assert!(got.is_some());
        let got = got.unwrap_or_default();
        prop_assert!(got.ends_with(&src[b..]));
        // 行の途中にある sorry を複数行のスクリプトで置き換えるときは、sorry の手前の
        // 空白を取り除き、スクリプトを by の次の行から書く。
        prop_assert!(got.starts_with(src[..a].trim_end_matches([' ', '\t'])));
    }
}

mod ファイルの_import {
    //! :save のヘッダに書き戻すため、読み込んだファイルの import を記録する。
    use super::*;

    #[story("B3", "B4")]
    #[test]
    fn import_の行だけを取り出す() {
        let src = "import Lean.Elab\nimport  Std\n\ndef a := 1\n-- import Nope\n";
        assert_eq!(pure::import_lines(src), ["Lean.Elab", "Std"]);
    }

    #[story("B3")]
    #[test]
    fn import_が無ければ空() {
        assert!(pure::import_lines("def a := 1\nimport\n").is_empty());
    }

    #[story("B3", "B4")]
    #[test]
    fn コメントの中の_import_は数えない() {
        // ヘッダに書くと repl はヘッダ全体を捨てて起動するので、書き出したファイルは
        // :l でも lean でもエラーになる。import 行を取り除く処理も doc コメントの
        // 中身を変える。
        let src = "/-\nimport Nope.NotAModule\n-/\n-- import Also.Not\nimport Std\n";
        assert_eq!(pure::import_lines(src), ["Std"]);
        assert!(pure::strip_imports(src).contains("import Nope.NotAModule"));
        assert!(!pure::strip_imports(src).contains("import Std"));
    }

    #[story("B3")]
    #[test]
    fn 宣言のあとの_import_は数えない() {
        // Lean も先頭以外の import を認めない。数えると :save のヘッダが増える。
        assert!(pure::import_lines("def a := 1\nimport Std\n").is_empty());
    }

    #[story("B4")]
    #[test]
    fn 入れ子のコメントの終わりを正しく判定する() {
        let src = "/- /- import In.Nest -/ import Still.In -/\nimport Real\n";
        assert_eq!(pure::import_lines(src), ["Real"]);
    }

    #[story("B3", "B4")]
    #[proptest]
    fn 取り除くのは本物の_import_行だけ(
        #[strategy(prop::collection::vec(prop::sample::select(HEADERS.to_vec()), 0..=12))]
        lines: Vec<&'static str>,
    ) {
        let src = lines.join("\n");
        let (mods, at) = pure::scan_header(&src);
        let stripped = pure::strip_imports(&src);

        prop_assert_eq!(mods.len(), at.len());
        // コメントの中の import を取り出していない。取り出すと :save した
        // ファイルがおかしくなる。
        prop_assert!(
            at.iter()
                .all(|&n| lines[n].split_whitespace().next() == Some("import"))
        );
        // 取り除いたあとに新しい import 行が現れることはない。ヘッダの終わりは
        // 変わらないので、1 回ですべて取り除ける。
        prop_assert!(pure::import_lines(&stripped).is_empty());
    }
}

mod 略記の展開 {
    //! `\name` を Lean の記号に変換する。変換は space か Tab で確定し、キー入力の
    //! たびには確認しない。
    use super::*;

    #[story("C5")]
    #[test]
    fn 名前を記号に変換する() {
        assert_eq!(expand_abbrev("a \\to"), Some(("→", 3)));
        assert_eq!(expand_abbrev("x\\dot"), Some(("·", 4)));
    }

    #[story("C5")]
    #[test]
    fn 長い略記が短い略記として変換されない() {
        // `\a` も `\all` も `\alpha` も表にある。キー入力のたびに確定すると `\a` の
        // 時点で変換されてしまい、長いほうを入力できなくなる。
        assert_eq!(expand_abbrev("\\a"), Some(("α", 2)));
        assert_eq!(expand_abbrev("\\all"), Some(("∀", 4)));
        assert_eq!(expand_abbrev("\\alpha"), Some(("α", 6)));
    }

    #[story("C5")]
    #[test]
    fn めったに使わない略記も変換できる() {
        // 表には VS Code の Lean 拡張の略記をほぼすべて (1829 件) 含めている。
        assert_eq!(expand_abbrev("\\frown"), Some(("⌢", 6)));
        assert_eq!(expand_abbrev("\\Gangia"), Some(("Ϫ", 7)));
    }

    #[story("C5")]
    #[test]
    fn 表に無い名前は変えない() {
        assert_eq!(expand_abbrev("\\nosuch"), None);
        assert_eq!(expand_abbrev("1 + 1"), None);
    }

    #[story("C5")]
    #[test]
    fn 略記のあとに記号が続く場合は変換しない() {
        // `\to)` 全体で表を検索する。`"\t"` のような文字列を ▸ に変換しない。
        assert_eq!(expand_abbrev("(\\to)"), None);
        assert_eq!(expand_abbrev("\"\\t\""), None);
    }

    #[story("C5")]
    #[proptest]
    fn 表のどの略記も変換できる(
        #[strategy(abbrev_keys())] key: &'static str,
        #[strategy("[^\\\\]{0,8}")] head: String,
    ) {
        // 例に書けるのは数件。1829 件すべてが同じ規則で変換できることはここで確認する。
        // 入力位置の手前に何が書いてあっても、直前の `\` より後ろだけをキーにする。
        let want = (ABBREV[key].as_str(), key.len() + 1);
        prop_assert_eq!(expand_abbrev(&format!("{head}\\{key}")), Some(want));
    }
}

mod 略記の候補 {
    //! Tab で略記を補完するときに、カーソルの手前の `\name` から記号の候補を作る。
    use super::*;

    #[story("C8", "J2")]
    #[test]
    fn カーソルの手前の略記の名前を取り出す() {
        assert_eq!(abbrev_head("theorem x : p \\to"), Some("to"));
        assert_eq!(abbrev_head("(\\."), Some("."));
        assert_eq!(abbrev_head("\\"), Some(""));
    }

    #[story("C8", "J2")]
    #[test]
    fn 略記の途中でなければ名前を取り出さない() {
        assert_eq!(abbrev_head("Nat.suc"), None);
        assert_eq!(abbrev_head("\\to x"), None, "空白のあとは略記ではない");
    }

    #[story("C8", "J2")]
    #[test]
    fn 一致する略記の記号を先頭に置く() {
        // `\a` は α になる。`\all` の ∀ などはそのあとに続く。
        let got = abbrev_candidates("a");
        assert_eq!(got.first(), Some(&"α"), "{got:?}");
        assert!(got.contains(&"∀"), "{got:?}");
    }
}

mod 定理検索 {
    //! loogle に問い合わせた結果を表示用の形に整形する。
    //! 外部と通信する部分だけが副作用を持ち、整形は純粋関数で行う。
    use super::*;

    fn hits() -> Json {
        json!([
            {
                "name": "List.map",
                "type": " (f : α → β) : List α → List β",
                "module": "Init.Prelude",
            },
            {
                "name": "List.mapTR",
                "type": " (f : α → β) : List α → List β",
                "module": "Init.Data.List.Basic",
            },
        ])
    }

    /// 10 件、幅 100 で整形する。
    fn text(got: &Json) -> String {
        pure::loogle_text(got, 10, 100, Style::PLAIN)
    }

    #[story("D4")]
    #[test]
    fn 名前と型と定義されている_module_を表示する() {
        let out = text(&json!({"count": 2, "hits": hits()}));
        let out: Vec<&str> = out.split('\n').collect();
        assert_eq!(out[0], "2 件");
        // `type` は先頭に空白が付いているので、`name : type` の形に整形する。
        assert_eq!(out[1], "List.map : (f : α → β) : List α → List β");
        // どの module にあるかも表示する。今の環境に無い名前も結果に含まれるため。
        assert_eq!(out[2], "  Init.Prelude");
        assert!(out[3].starts_with("List.mapTR : "), "{}", out[3]);
    }

    #[story("D4")]
    #[test]
    fn 件数を絞ったときは全体の件数も表示する() {
        // loogle は結果を 200 件で打ち切って返すので、count は hits より多いことがある。
        let got = json!({"count": 360, "hits": hits()});
        let head = pure::loogle_text(&got, 1, 100, Style::PLAIN);
        assert_eq!(head.split('\n').next(), Some("360 件 (先頭 1 件)"));
        let all = text(&json!({"count": 2, "hits": hits()}));
        assert_eq!(all.matches('\n').count(), 4);
    }

    #[story("D4")]
    #[test]
    fn 長い行は端末の幅で切り詰める() {
        // 折り返すと 1 件 2 行の並びが崩れて、どの型がどの名前のものか分からない。
        let got = json!({
            "count": 1,
            "hits": [{"name": "X", "type": format!(" {}", "a".repeat(200)), "module": "M"}],
        });
        let out = pure::loogle_text(&got, 10, 40, Style::PLAIN);
        let line = out.split('\n').nth(1).unwrap_or("");
        assert_eq!(line.chars().count(), 40);
        assert!(line.ends_with('…'), "{line}");
    }

    #[story("D4")]
    #[test]
    fn 見つからなければその旨を表示する() {
        assert_eq!(text(&json!({"count": 0, "hits": []})), "見つからなかった");
    }

    #[story("D4")]
    #[test]
    fn エラーは翻訳せずに候補を添える() {
        // loogle の文言は Lean のパーサのもの。日本語にすると元の位置情報が消える。
        let got = json!({"error": "unknown identifier 'Nope'", "suggestions": ["\"Nope\""]});
        let out = text(&got);
        assert_eq!(
            out.split('\n').collect::<Vec<_>>(),
            ["loogle: unknown identifier 'Nope'", "もしかして: \"Nope\""]
        );
    }

    /// 1 回だけ応答するローカルの HTTP サーバ。
    /// URL と、受け取ったリクエストの 1 行目を返す受け口を返す。
    fn serve(status: &'static str, body: &'static str) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/json", listener.local_addr().expect("addr"));
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(sock.try_clone().expect("clone"));
            let mut first = String::new();
            reader.read_line(&mut first).expect("request line");
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 0) && line.trim() != "" {
                line.clear();
            }
            let _ = write!(
                sock,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = tx.send(first.trim_end().to_string());
        });
        (url, rx)
    }

    fn ask(url: &str, query: &str) -> Result<Json, SearchError> {
        search::loogle(url, query, Duration::from_secs(5))
    }

    #[story("D4")]
    #[test]
    fn 記号を含むクエリも_URL_エンコードして送る() {
        // `?a + ?b` の `+` をそのまま URL に含めると、loogle 側では空白として
        // 解釈される。
        let (url, got) = serve("200 OK", r#"{"count": 0, "hits": []}"#);
        ask(&url, "?a + ?b").expect("loogle");
        let line = got.recv().expect("request");
        assert!(line.starts_with("GET /json?q=%3Fa%20%2B%20%3Fb "), "{line}");
    }

    #[story("D4")]
    #[test]
    fn 応答が無ければ_SearchError() {
        // ネットワークの問題で REPL を終了させない。呼び出し側がエラーを受け取って
        // 1 行で報告する。
        let closed = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/json", closed.local_addr().expect("addr"));
        drop(closed);
        assert!(ask(&url, "Nat").is_err());
    }

    #[story("D4", "F4")]
    #[test]
    fn HTTP_のエラーはコードと理由を_SearchError_にする() {
        let (url, _) = serve("503 Service Unavailable", "");
        let err = ask(&url, "Nat").expect_err("503");
        assert_eq!(err.to_string(), "503 Service Unavailable");
    }

    #[story("D4", "F4")]
    #[test]
    fn 応答がオブジェクトでなければ_SearchError() {
        let (url, _) = serve("200 OK", "[1, 2]");
        let err = ask(&url, "Nat").expect_err("配列");
        assert!(err.to_string().contains("オブジェクトではない"), "{err}");
    }
}

mod 性質一覧 {
    //! SPEC.md はテスト名から生成する (crates/spec)。手で書き足す場所を
    //! 作らない代わりに、生成し忘れをこのテストで検出する。
    use super::*;

    #[story("X1")]
    #[test]
    fn SPEC_md_がテストとそろっている() {
        if let Err(why) = spec::check(&spec::root()) {
            panic!("{why}");
        }
    }
}

mod Lake_プロジェクトの判定 {
    //! 設定に環境が無いとき、カレントディレクトリから Lake プロジェクトを探す。
    use super::*;

    fn libs(dir: &Path, file: &str, text: &str) -> Vec<String> {
        fs::write(dir.join(file), text).expect("write");
        config::lake_libs(&path_str(dir))
    }

    #[story("G6")]
    #[test]
    fn lakefile_を持つ一番近い親ディレクトリを返す() {
        let dir = tmp();
        fs::write(dir.path().join("lakefile.toml"), "").expect("write");
        let sub = dir.path().join("A").join("B");
        fs::create_dir_all(&sub).expect("mkdir");
        assert_eq!(
            config::lake_root(&path_str(&sub)),
            Some(path_str(dir.path()))
        );
    }

    #[story("G3", "G6")]
    #[test]
    fn lakefile_が無ければ_None_を返す() {
        let dir = tmp();
        assert_eq!(config::lake_root(&path_str(dir.path())), None);
    }

    #[story("G6")]
    #[test]
    fn lakefile_toml_の_lean_lib_を返す() {
        let dir = tmp();
        let text = "name = \"p\"\n[[lean_lib]]\nname = \"Foo\"\n[[lean_lib]]\nname = \"Bar\"\n";
        assert_eq!(libs(dir.path(), "lakefile.toml", text), ["Foo", "Bar"]);
    }

    #[story("G6")]
    #[test]
    fn lakefile_lean_の_lean_lib_を返す() {
        let dir = tmp();
        let text = "import Lake\nopen Lake DSL\n\nlean_lib Foo where\nlean_lib «Bar.Baz»\n";
        assert_eq!(libs(dir.path(), "lakefile.lean", text), ["Foo", "Bar.Baz"]);
    }

    #[story("G4", "G6")]
    #[test]
    fn TOML_として読めない_lakefile_toml_からは空を返す() {
        let dir = tmp();
        assert!(libs(dir.path(), "lakefile.toml", "[[lean_lib]\n").is_empty());
    }

    #[story("G6")]
    #[test]
    fn 設定が無ければ_Lake_プロジェクトの環境で起動する() {
        let dir = tmp();
        let proj = dir.path().join("myproj");
        fs::create_dir_all(proj.join("sub")).expect("mkdir");
        fs::write(
            proj.join("lakefile.toml"),
            "[[lean_lib]]\nname = \"MyProj\"\n",
        )
        .expect("write");
        let ctx = ctx_at(&path_str(&proj.join("sub")));
        let got = config::resolve(&ctx, None, None, &[], &table("")).expect("resolve");
        assert_eq!(
            (got.name.as_str(), got.project, got.imports),
            ("myproj", Some(path_str(&proj)), vec!["MyProj".to_string()])
        );
    }

    #[story("G3", "G6")]
    #[test]
    fn Lake_プロジェクトの外なら_Lean_本体だけで起動する() {
        let dir = tmp();
        let ctx = ctx_at(&path_str(dir.path()));
        let got = config::resolve(&ctx, None, None, &[], &table("")).expect("resolve");
        assert_eq!((got.name.as_str(), got.project), ("plain", None));
    }

    #[story("G6")]
    #[test]
    fn import_を指定したら_lean_lib_の代わりにそれを使う() {
        let dir = tmp();
        fs::write(
            dir.path().join("lakefile.toml"),
            "[[lean_lib]]\nname = \"Mine\"\n",
        )
        .expect("write");
        let ctx = ctx_at(&path_str(dir.path()));
        let imports = ["Std".to_string()];
        let got = config::resolve(&ctx, None, None, &imports, &table("")).expect("resolve");
        assert_eq!(got.imports, ["Std"]);
    }
}

mod 起動オプション {
    use super::*;

    fn parse(argv: &[&str]) -> Args {
        parse_args(argv).expect("parse_args")
    }

    #[story("G6")]
    #[test]
    fn dash_i_を繰り返すと_import_がすべて残る() {
        assert_eq!(parse(&["-i", "A", "--import", "B"]).imports, ["A", "B"]);
    }

    #[story("G5")]
    #[test]
    fn dash_e_で環境の名前を受け取る() {
        assert_eq!(parse(&["-e", "m"]).name.as_deref(), Some("m"));
        assert_eq!(parse(&["--env", "m"]).name.as_deref(), Some("m"));
    }

    #[story("G5", "G6")]
    #[test]
    fn 値の無いオプションはエラーで終了する() {
        // 終了コードは bin が決める。ここではメッセージだけを確かめる。
        for flag in ["-e", "-i", "-p"] {
            let why = parse_args(&[flag]).expect_err("エラーにならない");
            assert!(why.contains(&format!("{flag} には")), "{why}");
        }
    }

    #[story("H1")]
    #[test]
    fn 不明なオプションはエラーで終了する() {
        let why = parse_args(&["--nosuch"]).expect_err("エラーにならない");
        assert!(why.contains("不明なオプション: --nosuch"), "{why}");
    }

    #[story("B4")]
    #[test]
    fn ファイル名と_ハイフンは読み込むファイルとして受け取る() {
        assert_eq!(parse(&["foo.lean"]).preload.as_deref(), Some("foo.lean"));
        assert_eq!(parse(&["-"]).preload.as_deref(), Some("-"));
    }
}

mod 起動 {
    //! cli::main。`leani` コマンドを起動して、終了コードと表示を確かめる。エンジンの
    //! 用意は、偽の elan、lake、git を PATH に置いて差し替える。
    use super::*;
    use std::process::{Command, Stdio};

    /// コマンドの終了コードと、stdout と stderr の表示。
    struct Ran {
        code: i32,
        out: String,
        err: String,
    }

    /// input を stdin に渡してコマンドを動かし、終わるまで待つ。
    fn run(cmd: &mut Command, input: &str) -> Ran {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(input.as_bytes()).expect("write stdin");
        drop(stdin);
        let got = child.wait_with_output().expect("wait");
        Ran {
            code: got.status.code().unwrap_or(-1),
            out: String::from_utf8_lossy(&got.stdout).into_owned(),
            err: String::from_utf8_lossy(&got.stderr).into_owned(),
        }
    }

    /// 環境変数を vars だけにした `leani`。設定、履歴、init、state は dir の下に向ける。
    fn isolated(dir: &Path, vars: &[(&str, &str)]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_leani"));
        cmd.env_clear()
            .env("HOME", dir)
            .env("XDG_STATE_HOME", dir.join("state"))
            .env("LEANI_CONFIG", dir.join("config.toml"))
            .env("LEANI_HISTORY", dir.join("history"))
            .env("LEANI_INIT", dir.join("no-such-init.lean"))
            .envs(vars.iter().copied())
            .current_dir(dir);
        cmd
    }

    /// 本物のエンジンを使う `leani`。PATH と HOME はプロセスのものを使い、設定、
    /// 履歴、init だけを dir の下に向ける (engine.rs の sandbox と同じ)。
    fn real(dir: &Path) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_leani"));
        cmd.env("LEANI_CONFIG", dir.join("config.toml"))
            .env("LEANI_HISTORY", dir.join("history"))
            .env("LEANI_INIT", dir.join("no-such-init.lean"))
            .env_remove("LEANI_ENGINE")
            .env_remove("LEANI_NO_SETUP")
            .current_dir(dir);
        cmd
    }

    /// 設定ファイルを dir に書き、そのパスを返す。
    fn config(dir: &Path, text: &str) -> String {
        let path = dir.join("config.toml");
        fs::write(&path, text).expect("write config");
        path_str(&path)
    }

    fn with_engine(eng: &str) -> String {
        format!("default = \"t\"\n[env.t]\nimports = []\nengine = \"{eng}\"\n")
    }

    const NO_ENGINE: &str = "default = \"t\"\n[env.t]\nimports = []\n";

    #[story("G3")]
    #[test]
    fn dash_dash_setup_は用意済みならエンジンを用意しない() {
        let dir = tmp();
        let eng = built_engine(dir.path(), "leanprover/lean4:v1");
        config(dir.path(), &with_engine(&eng));
        let git = fake_git(&dir.path().join("git.log"), &[]);
        let path = fake_tools(dir.path(), &[("elan", ""), ("lake", ""), ("git", &git)]);

        let got = run(isolated(dir.path(), &[("PATH", &path)]).arg("--setup"), "");

        assert_eq!(got.code, 0, "{}", got.err);
        assert!(got.out.contains("用意済み"), "{}", got.out);
        assert_eq!(git_log(dir.path()), "", "エンジンを用意してしまった");
    }

    #[story("G3")]
    #[test]
    fn dash_dash_setup_は未用意ならエンジンを用意する() {
        let dir = tmp();
        config(dir.path(), NO_ENGINE);
        let git = fake_git(&dir.path().join("git.log"), &["v4.1.0"]);
        let lake = fake_lake(&dir.path().join("lake.count"));
        let lean = "echo 'Lean (version 4.1.0, arm64-apple-darwin, commit 0, Release)'";
        let path = fake_tools(
            dir.path(),
            &[("elan", ""), ("git", &git), ("lake", &lake), ("lean", lean)],
        );

        // --setup で明示的に頼まれたときは、LEANI_NO_SETUP があっても用意する。
        let vars = [("PATH", path.as_str()), ("LEANI_NO_SETUP", "1")];
        let got = run(isolated(dir.path(), &vars).arg("--setup"), "");

        assert_eq!(got.code, 0, "{}", got.err);
        assert!(
            git_log(dir.path()).contains("clone"),
            "エンジンを用意していない"
        );
        let repl = dir
            .path()
            .join("state/leani/engine/v4.1.0/.lake/build/bin/repl");
        assert!(repl.is_file(), "repl が無い: {}", repl.display());
    }

    #[story("G4")]
    #[test]
    fn エンジンが起動しなければメッセージを表示して_exit_1_で終わる() {
        let dir = tmp();
        let eng = built_engine(dir.path(), "leanprover/lean4:v1");
        config(dir.path(), &with_engine(&eng));
        // elan がすぐ終了するので、エンジンは最初の応答を返さずに終了する。
        let path = fake_tools(dir.path(), &[("elan", "exit 1"), ("lake", "")]);

        let got = run(&mut isolated(dir.path(), &[("PATH", &path)]), "");

        assert_eq!(got.code, 1, "{}", got.out);
        assert!(got.err.contains("エンジンが起動しなかった"), "{}", got.err);
    }

    #[story("G4")]
    #[test]
    fn 設定のエラーでは起動せずに知らせる() {
        let dir = tmp();
        config(dir.path(), "default = \n");
        let got = run(&mut isolated(dir.path(), &[]), "");
        assert_eq!(got.code, 1, "{}", got.out);
        assert!(got.err.contains("設定ファイルを読めない"), "{}", got.err);

        // 設定が読めても、起動できない理由があれば同じく知らせる。
        config(dir.path(), NO_ENGINE);
        let path = tools_path(dir.path(), &[]);
        let got = run(&mut isolated(dir.path(), &[("PATH", &path)]), "");
        assert_eq!(got.code, 1, "{}", got.out);
        assert!(got.err.contains("elan が PATH に無い"), "{}", got.err);
        assert!(
            !got.out.contains("import Lean"),
            "起動してしまった: {}",
            got.out
        );
    }

    #[story("B4")]
    #[test]
    fn 指定したファイルを_Repl_に渡す() {
        let dir = tmp();
        config(dir.path(), NO_ENGINE);
        let file = dir.path().join("given.lean");
        fs::write(&file, "def given := 12\n").expect("write");

        let got = run(real(dir.path()).arg(&file), "given + 1\n");

        assert_eq!(got.code, 0, "{}", got.err);
        // パイプの入力は表示されないので、プロンプトと結果が同じ行に並ぶ。
        let answered = got.out.lines().any(|l| l.trim_start_matches("λ> ") == "13");
        assert!(answered, "{}", got.out);
    }

    #[story("G7")]
    #[test]
    fn dash_V_は環境とバージョンと設定ファイルの場所を表示して終わる() {
        let dir = tmp();
        let eng = toolchain_dir(dir.path(), "eng", "leanprover/lean4:v1");
        let conf = config(dir.path(), &with_engine(&eng));

        let got = run(isolated(dir.path(), &[]).arg("-V"), "");

        assert_eq!(got.code, 0, "{}", got.err);
        assert!(got.out.contains("環境:     t (import Lean)"), "{}", got.out);
        assert!(
            got.out.contains("Lean:     leanprover/lean4:v1"),
            "{}",
            got.out
        );
        assert!(got.out.contains(&conf), "{}", got.out);
    }
}

mod 略記の一覧 {
    //! `:abbrev` の表示。`man ascii` のように、キーと記号の組を複数の列に並べる。
    use super::*;

    #[story("C7")]
    #[test]
    fn キーの先頭で絞り込む() {
        let got = abbrev_lookup("le");
        assert!(got.contains(&("le", "≤")), "{got:?}");
        assert!(got.iter().all(|(k, _)| k.starts_with("le")), "{got:?}");
        assert_eq!(abbrev_lookup("\\le"), got);
    }

    #[story("C7")]
    #[test]
    fn 記号からその記号を入力する略記を調べる() {
        let keys: Vec<&str> = abbrev_lookup("→").into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["->", "imp", "r", "r-", "rightarrow", "to"]);
    }

    #[story("C7")]
    #[test]
    fn 列の下まで並べたら右の列に続ける() {
        let pairs = [("a", "α"), ("b", "β"), ("c", "γ")];
        let got = abbrev_table(&pairs, 13);
        assert_eq!(
            got.split('\n').collect::<Vec<_>>(),
            ["\\a α   \\c γ", "\\b β"]
        );
    }

    #[story("C7")]
    #[test]
    fn 全角の文字は_2_文字分の幅で列を揃える() {
        let pairs = [("x", "あ"), ("y", "b"), ("z", "c"), ("w", "d")];
        let got = abbrev_table(&pairs, 12);
        assert_eq!(
            got.split('\n').collect::<Vec<_>>(),
            ["\\x あ   \\z c", "\\y b    \\w d"]
        );
    }

    #[story("C7")]
    #[test]
    fn 幅に収まる範囲で列を増やす() {
        let pairs = abbrev_lookup("");
        let rows: Vec<usize> = [40, 80, 120]
            .into_iter()
            .map(|width| {
                let table = abbrev_table(&pairs, width);
                let lines: Vec<&str> = table.split('\n').collect();
                assert!(lines.iter().all(|line| cell_width(line) <= width));
                lines.len()
            })
            .collect();
        // 40 では、いちばん長いキー (25 文字) の列と並べると幅を超えるので 1 列になる。
        let n = pairs.len();
        assert_eq!(rows, [n, n.div_ceil(2), n.div_ceil(4)]);
    }
}
