//! 入力をどう解釈するかを決める純粋関数と、読み取りのテスト。端末もエンジンも
//! 使わないのでミリ秒で終わる。

use leani::{boot, search};
use leani_core::abbrev::{
    ABBREV, abbrev_candidates, abbrev_head, abbrev_lookup, abbrev_table, cell_width, expand_abbrev,
};
use leani_core::pure::{self, PAIRS, Style};
use leani_core::queries::{BLOCK_OPEN, DECL_NAME, INCOMPLETE};
use leani_core::types::{EngineFault, Json, Kind, Proof, SearchError, State, Target};
use leani_read::args::{Args, parse_args};
use leani_read::config::{self, EnvConfig};
use leani_read::places::Ctx;
use proptest::prelude::*;
use serde_json::json;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
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
const HEADERS: [&str; 14] = [
    "import Std",
    "import Lean.Elab",
    "  import Mathlib",
    "import",
    "import Foo -- メモ",
    "module",
    "public import Lean",
    "public meta import all Qux",
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

mod judging_continuation_lines {
    //! ja: 継続行の判定
    //! en: Judging continuation lines
    //!
    //! 単独では入力になりえない行を見分ける。
    use super::*;

    /// ja: インデントした行を前の行の続きと判定する
    /// en: Judges an indented line as a continuation of the previous line
    #[story("C1")]
    #[test]
    fn judges_an_indented_line_as_a_continuation_of_the_previous_line() {
        for line in ["  | 0 => 0", "\tfoo", "  bar"] {
            assert!(pure::continues(line), "継続にならない: {line:?}");
        }
    }

    /// ja: 行頭がパイプの行を前の行の続きと判定する
    /// en: Judges a line starting with a pipe as a continuation of the previous line
    #[story("C1")]
    #[test]
    fn judges_a_line_starting_with_a_pipe_as_a_continuation_of_the_previous_line() {
        // `|` で始まる command も tactic も Lean には無い。
        assert!(pure::continues("| zero => rfl"));
    }

    /// ja: インデントもパイプも無い行を単独の入力と判定する
    /// en: Judges a line without indentation or a leading pipe as a separate input
    #[story("A1", "C1")]
    #[test]
    fn judges_a_line_without_indentation_or_a_leading_pipe_as_a_separate_input() {
        for line in ["def f := 1", "#eval 1", "", "x + 1"] {
            assert!(!pure::continues(line), "継続にされた: {line:?}");
        }
    }
}

mod lines_that_wait_for_more_input {
    //! ja: 続きを待つ行
    //! en: Lines that wait for more input
    //!
    //! 完結していても次の行がありうる構文。
    use super::*;

    /// ja: where、with、do、by で終わる行を、続きを待つ行と判定する
    /// en: Judges a line ending in where, with, do or by as a line that waits for more
    #[story("C1")]
    #[test]
    fn judges_a_line_ending_in_where_with_do_or_by_as_a_line_that_waits_for_more() {
        for line in [
            "structure P where",
            "induction n with",
            "def f := do",
            "theorem t : True := by",
        ] {
            assert!(BLOCK_OPEN.is_match(line), "待たない: {line:?}");
        }
    }

    /// ja: where や by を語の一部として含むだけの行は、待たない行と判定する
    /// en: Judges a line with where or by only inside a word as a line that does not wait
    #[story("C1")]
    #[test]
    fn judges_a_line_with_where_or_by_only_inside_a_word_as_a_line_that_does_not_wait() {
        for line in ["def where_ := 1", "#eval byte", "def f := 1"] {
            assert!(!BLOCK_OPEN.is_match(line), "余計に待つ: {line:?}");
        }
    }
}

mod responses_from_the_parser {
    //! ja: パーサからの応答
    //! en: Responses from the parser
    //!
    //! 「まだ途中」という応答だけを継続とみなす。
    use super::*;

    /// ja: 入力途中だという応答を、次の行を待つと判定する
    /// en: Judges a response about unfinished input as waiting for the next line
    #[story("C1")]
    #[test]
    fn judges_a_response_about_unfinished_input_as_waiting_for_the_next_line() {
        assert!(INCOMPLETE.is_match("<input>:1:5: unexpected end of input"));
        assert!(INCOMPLETE.is_match("unterminated comment"));
    }

    /// ja: 閉じていない raw 文字列か名前のエスケープの応答を、次の行を待つと判定する
    /// en: Judges a response about an unterminated raw string or name escape as waiting for the next line
    #[story("C6")]
    #[test]
    fn judges_a_response_about_an_unterminated_raw_string_or_name_escape_as_waiting_for_the_next_line()
     {
        assert!(INCOMPLETE.is_match("<input>:1:9: unterminated raw string literal"));
        assert!(INCOMPLETE.is_match("<input>:1:8: unterminated identifier escape"));
    }

    /// ja: 入力途中ではないエラーの応答を、待たずに送ると判定する
    /// en: Judges an error response not about unfinished input as ready to send
    #[story("C1")]
    #[test]
    fn judges_an_error_response_not_about_unfinished_input_as_ready_to_send() {
        assert!(!INCOMPLETE.is_match("unknown identifier 'foo'"));
    }
}

mod tactic_suggestions {
    //! ja: タクティクの提案
    //! en: Tactic suggestions
    //!
    //! exact? や simp? は結果を "Try this:" として返す。
    use super::*;

    /// ja: 提案から [apply] の印を除いたタクティクを返す
    /// en: Returns the tactic without the [apply] mark from a suggestion
    #[story("E2")]
    #[test]
    fn returns_the_tactic_without_the_apply_mark_from_a_suggestion() {
        let found = pure::try_this(&[
            json!({"severity": "info", "data": "Try this:\n  [apply] exact Nat.le_refl n"}),
        ]);
        assert_eq!(found.as_deref(), Some("exact Nat.le_refl n"));
    }

    /// ja: 折り返した提案から、すべての行を返す
    /// en: Returns every line of a suggestion that wraps over several lines
    #[story("E2")]
    #[test]
    fn returns_every_line_of_a_suggestion_that_wraps_over_several_lines() {
        // pretty printer が 100 桁前後で折り返す。1 行目だけ取り出すと
        // `simp only [a, b,` になり、それが完成した証明として表示されてしまう。
        let found = pure::try_this(&[json!({
            "severity": "info",
            "data": "Try this:\n  [apply] simp only [aaa, bbb,\n    ccc, ddd]",
        })]);
        assert_eq!(found.as_deref(), Some("simp only [aaa, bbb,\n  ccc, ddd]"));
    }

    /// ja: Try this と同じ行に書いた提案を返す
    /// en: Returns a suggestion written on the same line as Try this
    #[story("E2")]
    #[test]
    fn returns_a_suggestion_written_on_the_same_line_as_try_this() {
        let found = pure::try_this(&[json!({"data": "Try this: exact Nat.le_refl n"})]);
        assert_eq!(found.as_deref(), Some("exact Nat.le_refl n"));
    }

    /// ja: 括弧が合わない提案を途中で切れていると判定し、合う提案を切れていないと判定する
    /// en: Judges a suggestion with unmatched brackets as cut off and one with matched brackets as whole
    #[story("E2")]
    #[test]
    fn judges_a_suggestion_with_unmatched_brackets_as_cut_off_and_one_with_matched_brackets_as_whole()
     {
        // スクリプトに追加する前の最後の確認。途中で切ると必ず括弧が合わなくなる。
        assert!(pure::balanced("simp only [aaa, bbb]"));
        assert!(pure::balanced("exact ⟨foo (bar x), rfl⟩"));
        assert!(!pure::balanced("simp only [aaa, bbb,"));
        assert!(!pure::balanced("exact foo)"));
        assert!(!pure::balanced("exact ⟨foo]"));
    }

    /// ja: 括弧が閉じた文字列をどう 2 つ連結しても閉じていると判定する
    /// en: Judges any concatenation of two balanced strings as balanced
    #[story("E2")]
    #[proptest]
    fn judges_any_concatenation_of_two_balanced_strings_as_balanced(
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

    /// ja: 提案の無いメッセージにも空の一覧にも None を返す
    /// en: Returns None for a message without a suggestion and for no messages
    #[story("E2")]
    #[test]
    fn returns_none_for_a_message_without_a_suggestion_and_for_no_messages() {
        let done = json!({"severity": "info", "data": "goals accomplished"});
        assert_eq!(pure::try_this(&[done]), None);
        assert_eq!(pure::try_this(&[]), None);
    }
}

mod extracting_the_name_of_a_declaration {
    //! ja: 宣言の名前の取り出し
    //! en: Extracting the name of a declaration
    //!
    //! ユーザーが実行した宣言を補完の候補に追加するために使う。
    use super::*;

    fn names(src: &str) -> Vec<&str> {
        DECL_NAME
            .captures_iter(src)
            .filter_map(|m| m.get(1).map(|g| g.as_str()))
            .collect()
    }

    /// ja: 修飾子や属性が付いた宣言でも、その名前を返す
    /// en: Returns the name of a declaration even with modifiers or attributes
    #[story("D1")]
    #[test]
    fn returns_the_name_of_a_declaration_even_with_modifiers_or_attributes() {
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

    /// ja: 宣言でない入力には名前を返さない
    /// en: Returns no names for an input that is not a declaration
    #[story("D1")]
    #[test]
    fn returns_no_names_for_an_input_that_is_not_a_declaration() {
        assert!(names("#eval 1 + 1").is_empty());
    }
}

mod cutting_out_the_name_to_complete {
    //! ja: 補完する名前の切り出し
    //! en: Cutting out the name to complete
    use super::*;

    /// ja: 区切り文字の直後から名前を切り出し、. では区切らない
    /// en: Cuts out the name after a delimiter without splitting at a dot
    #[story("D1")]
    #[test]
    fn cuts_out_the_name_after_a_delimiter_without_splitting_at_a_dot() {
        assert_eq!(pure::name_start("#check (Nat.suc"), "#check (".len());
        assert_eq!(pure::name_start("Nat.suc"), 0, "`.` で区切った");
    }

    /// ja: 名前の前に記号があれば、記号の後ろから名前を切り出す
    /// en: Cuts out the name after a symbol in front of it
    #[story("D1")]
    #[test]
    fn cuts_out_the_name_after_a_symbol_in_front_of_it() {
        // `@` や `¬` の直後から名前を補完する。区切り文字の一覧に無い記号を名前に
        // 含めると、`@Nat.su` を `@Nat.su` という名前として問い合わせ、候補が出ない。
        for (text, before) in [
            ("@Nat.su", "@"),
            ("¬Nat.su", "¬"),
            ("f ∘ Nat.su", "f ∘ "),
            ("x+Nat.su", "x+"),
            ("λx", "λ"),
            ("#check ⟨Nat.su", "#check ⟨"),
        ] {
            assert_eq!(pure::name_start(text), before.len(), "{text}");
        }
        // 名前に使える文字では区切らない。
        for text in ["Nat.succ_le'", "x₁", "List.map!", "α.β", "ℕ"] {
            assert_eq!(pure::name_start(text), 0, "{text}");
        }
        // `«` で囲んだ部分は名前の一部。閉じていなくても、カーソルまでを名前にする。
        assert_eq!(pure::name_start("#check Foo.«ba"), "#check ".len());
        assert_eq!(pure::name_start("(Foo.«a b».ba"), "(".len());
    }

    /// ja: 名前の前に Lean の名前に使えない文字があれば、その文字の後ろから名前を切り出す
    /// en: Cuts out the name after a character that Lean does not allow in names
    #[story("D1")]
    #[test]
    fn cuts_out_the_name_after_a_character_that_lean_does_not_allow_in_names() {
        // Lean の名前に使える文字は、Unicode の英数字より狭い。漢字や全角の英字を
        // 名前に含めると、`定理Nat.su` を 1 つの名前として問い合わせ、候補が出ない。
        for (text, before) in [("定理Nat.su", "定理"), ("①x", "①"), ("Ａx", "Ａ")] {
            assert_eq!(pure::name_start(text), before.len(), "{text}");
        }
        for text in ["café", "Ω₁", "𝔽"] {
            assert_eq!(pure::name_start(text), 0, "{text}");
        }
    }

    /// ja: とても長い名前でも、スタックを使い切らずに名前の先頭を返す
    /// en: Returns the start of a very long name without running out of stack
    #[story("D1")]
    #[test]
    fn returns_the_start_of_a_very_long_name_without_running_out_of_stack() {
        assert_eq!(pure::name_start(&"a".repeat(1_000_000)), 0);
        assert_eq!(pure::name_start(&"«a»".repeat(300_000)), 0);
    }

    /// ja: どの区切りの記号のあとに置いたどの名前も、そのまま切り出す
    /// en: Cuts out any name exactly as written after any separator symbol
    #[story("D1")]
    #[proptest]
    fn cuts_out_any_name_exactly_as_written_after_any_separator_symbol(
        #[strategy(prop::sample::select(vec!["@", "¬", "(", "⟨", " ", "+", "λ", "∘", "↦", "\""]))]
        sep: &'static str,
        #[strategy("[A-Za-zα-κμ-ω_][A-Za-z0-9_'!?.₀-₉]{0,8}")] name: String,
    ) {
        let text = format!("x{sep}{name}");
        prop_assert_eq!(&text[pure::name_start(&text)..], name.as_str());
    }
}

mod unit_for_getting_completion_candidates_together {
    //! ja: 補完の候補をまとめて取得する単位
    //! en: Unit for getting completion candidates together
    //!
    //! 名前空間があればそこまで、無ければ先頭 2 文字。
    use super::*;

    /// ja: 名前空間のある名前には、名前空間の . までを返す
    /// en: Returns the name up to the namespace dot for a name with a namespace
    #[story("D1")]
    #[test]
    fn returns_the_name_up_to_the_namespace_dot_for_a_name_with_a_namespace() {
        assert_eq!(pure::name_chunk("Nat.suc"), "Nat.");
        assert_eq!(
            pure::name_chunk("MeasureTheory.integral_"),
            "MeasureTheory."
        );
    }

    /// ja: 名前空間の無い名前には先頭 2 文字を返す
    /// en: Returns the first two characters for a name without a namespace
    #[story("D1")]
    #[test]
    fn returns_the_first_two_characters_for_a_name_without_a_namespace() {
        // Mathlib では 1 文字だと `C` で 7.5 万件になるので、単位を広げすぎない。
        assert_eq!(pure::name_chunk("Contin"), "Co");
    }
}

mod lean_string_literals {
    //! ja: Lean の文字列リテラル
    //! en: Lean string literals
    use super::*;

    /// ja: どの文字列からも、Lean が読めるエスケープだけを使ったリテラルを作る
    /// en: Builds a literal using only escapes that Lean reads from any string
    #[story("C1")]
    #[proptest]
    fn builds_a_literal_using_only_escapes_that_lean_reads_from_any_string(
        #[strategy("[\\x00-\\x1f\\x7f\\\\\"aあ]{0,12}")] s: String,
    ) {
        // Lean の文字列は \b や \f を受け付けない。読めないエスケープを書くと
        // 完結の判定がエラーになり、式を宣言として送ってしまう。
        let escape = regex::Regex::new(r#"\\(?:[\\"ntr]|u[0-9a-f]{4})"#).expect("正規表現");
        let lit = pure::lean_str(&s);
        let body = lit
            .strip_prefix('"')
            .and_then(|l| l.strip_suffix('"'))
            .expect("引用符で囲む");
        let rest = escape.replace_all(body, "");
        prop_assert!(
            !rest.contains(['\\', '"']) && !rest.chars().any(char::is_control),
            "{lit}"
        );
    }
}

mod json_in_the_engine_output {
    //! ja: エンジンの出力の JSON
    //! en: JSON in the engine output
    //!
    //! 問い合わせの結果はエンジンの出力の最後の行にある。
    use super::*;

    /// ja: 出力の最後の行を JSON として読んで返す
    /// en: Returns the last line of the output parsed as JSON
    #[story("D1", "C1")]
    #[test]
    fn returns_the_last_line_of_the_output_parsed_as_json() {
        let got = pure::last_json(Some("宣言が出力した行\n{\"a\": [1]}\n"));
        assert_eq!(got, Some(json!({"a": [1]})));
    }

    /// ja: 出力が無いか、空白だけか、最後の行が JSON でなければ None を返す
    /// en: Returns None if the output is missing or blank or its last line is not JSON
    #[story("D1", "C1")]
    #[test]
    fn returns_none_if_the_output_is_missing_or_blank_or_its_last_line_is_not_json() {
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

mod judging_whether_the_input_is_complete {
    //! ja: 入力が完結したかの判定
    //! en: Judging whether the input is complete
    //!
    //! パーサの応答 (JSON) だけを見て、送り方と状態を決める。
    use super::*;

    /// ja: command として読める入力を、完結した command と判定する
    /// en: Judges input that parses as a command as a complete command
    #[story("A1", "C1")]
    #[test]
    fn judges_input_that_parses_as_a_command_as_a_complete_command() {
        let probe = json!({"cmd": {"ok": true}, "term": {"ok": false, "err": "x"}});
        assert_eq!(pure::classify(Some(&probe)), (State::Complete, Kind::Cmd));
    }

    /// ja: term としてしか読めない入力を、完結した term と判定する
    /// en: Judges input that parses only as a term as a complete term
    #[story("A1")]
    #[test]
    fn judges_input_that_parses_only_as_a_term_as_a_complete_term() {
        let probe = json!({"cmd": {"ok": false, "err": "x"}, "term": {"ok": true}});
        assert_eq!(pure::classify(Some(&probe)), (State::Complete, Kind::Term));
    }

    /// ja: command として読むと途中で終わる入力は、続きを待つと判定する
    /// en: Judges input that ends early as a command as waiting for more lines
    #[story("C1")]
    #[test]
    fn judges_input_that_ends_early_as_a_command_as_waiting_for_more_lines() {
        let probe = json!({
            "cmd": {"ok": false, "err": "<input>:1:9: unexpected end of input"},
            "term": {"ok": false, "err": "<input>:1:3: unexpected token"},
        });
        assert_eq!(pure::classify(Some(&probe)), (State::More, Kind::Cmd));
    }

    /// ja: command としても term としても読めない入力は、より先まで読めたほうのエラーと判定する
    /// en: Judges input that parses as neither command nor term as an error of the kind that got further
    #[story("C1")]
    #[test]
    fn judges_input_that_parses_as_neither_command_nor_term_as_an_error_of_the_kind_that_got_further()
     {
        let probe = json!({
            "cmd": {"ok": false, "err": "<input>:1:2: unexpected token"},
            "term": {"ok": false, "err": "<input>:1:7: unexpected token"},
        });
        assert_eq!(pure::classify(Some(&probe)), (State::Err, Kind::Term));
    }

    /// ja: パーサの応答が無ければ、command のエラーと判定する
    /// en: Judges input as a command error when there is no parser response
    #[story("C1")]
    #[test]
    fn judges_input_as_a_command_error_when_there_is_no_parser_response() {
        // leani の推測でエラーを作らない。command として送る。
        assert_eq!(pure::classify(None), (State::Err, Kind::Cmd));
    }

    /// ja: 証明モードでは、タクティクの列として読めるかで完結、途中、エラーを判定する
    /// en: Judges input in proof mode as complete, unfinished or an error by parsing it as tactics
    #[story("E1")]
    #[test]
    fn judges_input_in_proof_mode_as_complete_unfinished_or_an_error_by_parsing_it_as_tactics() {
        let ok = json!({"tac": {"ok": true}});
        assert_eq!(pure::classify_tac(Some(&ok)), (State::Complete, Kind::Tac));
        let more = json!({"tac": {"ok": false, "err": "unexpected end of input"}});
        assert_eq!(pure::classify_tac(Some(&more)), (State::More, Kind::Tac));
        assert_eq!(pure::classify_tac(None), (State::Err, Kind::Tac));
    }

    /// ja: インデントした行で終わるブロックは空行を待ち、1 行の入力は待たないと判定する
    /// en: Judges a block ending in an indented line as waiting for a blank line and one line as not
    #[story("C1")]
    #[test]
    fn judges_a_block_ending_in_an_indented_line_as_waiting_for_a_blank_line_and_one_line_as_not() {
        assert!(pure::block_continues(
            &["def f", "  | 0 => 0"],
            "def f\n  | 0 => 0"
        ));
        assert!(!pure::block_continues(&["def f := 1"], "def f := 1"));
    }
}

mod reasons_a_value_cannot_be_evaluated {
    //! ja: 評価できない理由
    //! en: Reasons a value cannot be evaluated
    //!
    //! #eval のエラー文から、評価できない理由を取り出す。
    use super::*;

    /// ja: Repr のインスタンスが無いというエラーには、Repr を含む理由を返す
    /// en: Returns a reason mentioning Repr for an error about a missing Repr instance
    #[story("A2")]
    #[test]
    fn returns_a_reason_mentioning_repr_for_an_error_about_a_missing_repr_instance() {
        let blob = "Could not synthesize a `ToExpr`, `Repr`, or `ToString` instance";
        assert!(pure::not_evaluable_reason(blob).is_some_and(|why| why.contains("Repr")));
    }

    /// ja: noncomputable にするよう勧めるエラーには、noncomputable を含む理由を返す
    /// en: Returns a reason mentioning noncomputable for an error that suggests noncomputable
    #[story("A2")]
    #[test]
    fn returns_a_reason_mentioning_noncomputable_for_an_error_that_suggests_noncomputable() {
        let blob = "failed to compile definition, consider marking it as 'noncomputable'";
        assert!(pure::not_evaluable_reason(blob).is_some_and(|why| why.contains("noncomputable")));
    }

    /// ja: 評価できないことを表すエラーでなければ None を返す
    /// en: Returns None for an error unrelated to evaluation
    #[story("A2")]
    #[test]
    fn returns_none_for_an_error_unrelated_to_evaluation() {
        assert_eq!(pure::not_evaluable_reason("Unknown identifier `x`"), None);
    }
}

mod actions_outside_do {
    //! ja: do の外の action
    //! en: Actions outside do
    use super::*;

    /// ja: do の外に action を書いたというエラーを、do の外の action と判定する
    /// en: Judges an error about an action written outside do as an action outside do
    #[story("A1")]
    #[test]
    fn judges_an_error_about_an_action_written_outside_do_as_an_action_outside_do() {
        let blob = "Nested action `← f` must be nested inside a `do` expression.";
        assert!(pure::nested_action(blob));
    }

    /// ja: ほかのエラーを、do の外の action ではないと判定する
    /// en: Judges other errors as not about an action outside do
    #[story("A1")]
    #[test]
    fn judges_other_errors_as_not_about_an_action_outside_do() {
        assert!(!pure::nested_action("Unknown identifier `x`"));
    }
}

mod range_of_an_error_position {
    //! ja: エラー位置の範囲
    //! en: Range of an error position
    //!
    //! メッセージの位置を、送ったソースの座標からその行の中に収める。
    use super::*;

    /// ja: eval で包んでずれた行と桁を戻した範囲を返す
    /// en: Returns the range shifted back by the lines and columns that wrapping in eval added
    #[story("A5")]
    #[test]
    fn returns_the_range_shifted_back_by_the_lines_and_columns_that_wrapping_in_eval_added() {
        let m = json!({"pos": {"line": 2, "column": 6}, "endPos": {"line": 2, "column": 9}});
        // #eval で 1 行 2 桁ずらして送っているので、元のソースでは 1 行 4 桁。
        assert_eq!(pure::span(&m, &["foo bar"], 1, 2), Some((1, 4, 3)));
    }

    /// ja: 文字列の中で始まる行では、行だけを戻し、桁は戻さない
    /// en: Shifts back only the line, not the column, for a line that starts inside a string
    #[story("A5")]
    #[test]
    fn shifts_back_only_the_line_not_the_column_for_a_line_that_starts_inside_a_string() {
        // 2 行目は文字列の中で始まるので、`#eval` に包むときに空白を足していない。
        // そのため、桁はずれていない。
        let m = json!({"pos": {"line": 3, "column": 3}, "endPos": {"line": 3, "column": 5}});
        assert_eq!(pure::span(&m, &["f \"a", "b\".xy"], 1, 2), Some((2, 3, 2)));
    }

    /// ja: 位置が行の外なら None を返す
    /// en: Returns None for a position outside the lines
    #[story("A5")]
    #[test]
    fn returns_none_for_a_position_outside_the_lines() {
        let m = json!({"pos": {"line": 9, "column": 0}});
        assert_eq!(pure::span(&m, &["foo"], 0, 0), None);
    }

    /// ja: 範囲の幅を行の長さに収めて返す
    /// en: Limits the width of a range to the length of the line
    #[story("A5")]
    #[test]
    fn limits_the_width_of_a_range_to_the_length_of_the_line() {
        let m = json!({"pos": {"line": 1, "column": 1}, "endPos": {"line": 1, "column": 99}});
        assert_eq!(pure::span(&m, &["abc"], 0, 0), Some((1, 1, 2)));
    }

    /// ja: 全角文字を 2 桁と数えて、キャレットの位置と長さを決める
    /// en: Counts a full-width character as two columns for the caret position and length
    #[story("A5")]
    #[test]
    fn counts_a_full_width_character_as_two_columns_for_the_caret_position_and_length() {
        // 端末では全角文字が 2 桁を占めるので、文字の数だけずらすとキャレットが
        // 左にずれる。
        assert_eq!(pure::caret_pad("あい x", 3), " ".repeat(5));
        assert_eq!(pure::caret_len("x := あいう", 5, 3), 6);
        assert_eq!(pure::caret_len("abc", 1, 2), 2);
    }

    /// ja: キャレットの前のタブを、空白にせずタブのまま残す
    /// en: Keeps a tab before the caret as a tab instead of spaces
    #[story("A5")]
    #[test]
    fn keeps_a_tab_before_the_caret_as_a_tab_instead_of_spaces() {
        // タブの幅は端末が決めるので、同じ位置にタブを置けば列がそろう。
        assert_eq!(pure::caret_pad("\tfoo bar", 5), "\t    ");
    }
}

mod cutting_out_responses {
    //! ja: レスポンスの切り出し
    //! en: Cutting out responses
    //!
    //! 空行で区切られた JSON を、受信し終えた分だけ取り出す。
    use super::*;

    /// ja: 空行が届く前は None を返す
    /// en: Returns None before a blank line arrives
    #[story("A1")]
    #[test]
    fn returns_none_before_a_blank_line_arrives() {
        assert_eq!(pure::first_response("{\"env\": 1}"), None);
    }

    /// ja: 空行が届いたら、その前の JSON を読んで返す
    /// en: Returns the JSON before a blank line once the blank line arrives
    #[story("A1")]
    #[test]
    fn returns_the_json_before_a_blank_line_once_the_blank_line_arrives() {
        assert_eq!(
            pure::first_response("{\"env\": 1}\n\n"),
            Some(json!({"env": 1}))
        );
    }

    /// ja: 文字列の値に空行を含む JSON を、その値のまま読む
    /// en: Reads JSON with a blank line in a string value and keeps the value
    #[story("A1")]
    #[test]
    fn reads_json_with_a_blank_line_in_a_string_value_and_keeps_the_value() {
        let buf = "{\"messages\": [{\"data\": \"a\\n\\nb\"}]}\n\n";
        let got = pure::first_response(buf).expect("レスポンスを読めない");
        assert_eq!(got["messages"][0]["data"], "a\n\nb");
    }
}

mod panic_in_the_engine {
    //! ja: エンジンの PANIC
    //! en: PANIC in the engine
    //!
    //! エンジンが PANIC したかを、レスポンスのメッセージから判定する。
    use super::*;

    /// ja: PANIC したレスポンスには、バックトレースを除いた PANIC の行を返す
    /// en: Returns the PANIC line without the backtrace for a response with a PANIC
    #[story("F1")]
    #[test]
    fn returns_the_panic_line_without_the_backtrace_for_a_response_with_a_panic() {
        let resp = json!({"messages": [
            {"severity": "info", "data": "1"},
            {"severity": "info", "data": PANIC},
        ]});
        assert_eq!(
            pure::panic_line(&resp).as_deref(),
            Some("PANIC at Foo.bar Foo:1:2: oops")
        );
    }

    /// ja: 同じメッセージの中で PANIC の前に出力があっても、PANIC の行を返す
    /// en: Returns the PANIC line when output comes before it in the same message
    #[story("F1")]
    #[test]
    fn returns_the_panic_line_when_output_comes_before_it_in_the_same_message() {
        let resp = json!({"messages": [{"severity": "info", "data": format!("42\n{PANIC}")}]});
        assert_eq!(
            pure::panic_line(&resp).as_deref(),
            Some("PANIC at Foo.bar Foo:1:2: oops")
        );
    }

    /// ja: PANIC の無いレスポンスにも、メッセージの無いレスポンスにも None を返す
    /// en: Returns None for a response without a PANIC or without any messages
    #[story("F1")]
    #[test]
    fn returns_none_for_a_response_without_a_panic_or_without_any_messages() {
        let resp = json!({"messages": [{"severity": "info", "data": "2"}]});
        assert_eq!(pure::panic_line(&resp), None);
        assert_eq!(pure::panic_line(&json!({})), None);
    }
}

mod preparation_before_startup {
    //! ja: 起動前の準備
    //! en: Preparation before startup
    //!
    //! import を追加したり取り除いたりする処理。
    use super::*;

    /// ja: init ファイルの中身から import の行を取り除いて返す
    /// en: Returns the contents of an init file with its import lines removed
    #[story("G2")]
    #[test]
    fn returns_the_contents_of_an_init_file_with_its_import_lines_removed() {
        // init は既存の環境の上に追加するので、import を書けない。
        assert_eq!(pure::strip_imports("import Foo\ndef x := 1"), "def x := 1");
    }

    /// ja: import の行があるファイルだけを import を含むと判定し、コメントの中の import は見ない
    /// en: Judges a file as having imports by its import lines and not by import in a comment
    #[story("G2")]
    #[test]
    fn judges_a_file_as_having_imports_by_its_import_lines_and_not_by_import_in_a_comment() {
        assert!(pure::has_import("\nimport Mathlib\ndef f := 1"));
        assert!(!pure::has_import("def f := 1  -- import ではない"));
    }

    /// ja: lake env のキャッシュの行から、変数の名前と値の組を返す
    /// en: Returns the variable names and values from the lines of the lake env cache
    #[story("G1")]
    #[test]
    fn returns_the_variable_names_and_values_from_the_lines_of_the_lake_env_cache() {
        let env = pure::parse_env_lines("LEAN_PATH=/a:/b\nLD_LIBRARY_PATH=\n");
        let want = [("LEAN_PATH", "/a:/b"), ("LD_LIBRARY_PATH", "")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .into();
        assert_eq!(env, want);
    }

    /// ja: キャッシュより新しい lean-toolchain があれば、キャッシュが古いと判定する
    /// en: Judges the cache as stale when lean-toolchain is newer than the cache
    #[story("G4", "G6")]
    #[test]
    fn judges_the_cache_as_stale_when_lean_toolchain_is_newer_than_the_cache() {
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

mod engine_version {
    //! ja: エンジンのバージョン
    //! en: Engine version
    //!
    //! 使う Lean のバージョンから repl のタグとパスを決める。
    use super::*;

    const TAGS: [&str; 3] = ["v4.32.0", "v4.33.0", "v4.34.0-rc2"];

    /// ja: toolchain の名前からバージョンの部分だけを返す
    /// en: Returns only the version part of a toolchain name
    #[story("G3")]
    #[test]
    fn returns_only_the_version_part_of_a_toolchain_name() {
        assert_eq!(
            pure::toolchain_version("leanprover/lean4:v4.34.0-rc2"),
            "v4.34.0-rc2"
        );
        assert_eq!(pure::toolchain_version("v4.33.0"), "v4.33.0");
    }

    /// ja: 自分でビルドしたエンジンの toolchain が違えば、両方のバージョンとビルドし直すコマンドを含む警告を返す
    /// en: Returns a warning with both versions and the rebuild command for a self-built engine on another toolchain
    #[story("G4")]
    #[test]
    fn returns_a_warning_with_both_versions_and_the_rebuild_command_for_a_self_built_engine_on_another_toolchain()
     {
        let got = pure::toolchain_warning(true, "v4.33.0", "v4.34.0", "/eng").expect("警告しない");
        assert!(
            got.contains("使うバージョン=v4.34.0 / エンジン=v4.33.0"),
            "{got}"
        );
        assert!(got.contains("cd /eng && lake build repl"), "{got}");
    }

    /// ja: toolchain が同じか、leani が用意したエンジンか、toolchain を読めなければ None を返す
    /// en: Returns None if the toolchains match, leani set up the engine, or the engine toolchain is unreadable
    #[story("G4")]
    #[test]
    fn returns_none_if_the_toolchains_match_leani_set_up_the_engine_or_the_engine_toolchain_is_unreadable()
     {
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

    /// ja: 既定の配布元を省いた toolchain を、省かない toolchain とバージョンだけで比べる
    /// en: Compares a toolchain that omits the default repository with a full one by version only
    #[story("G3", "G4")]
    #[test]
    fn compares_a_toolchain_that_omits_the_default_repository_with_a_full_one_by_version_only() {
        // elan は `v4.34.0` を `leanprover/lean4:v4.34.0` と同じ toolchain として扱う。
        let short = "v4.34.0";
        let full = "leanprover/lean4:v4.34.0";
        assert_eq!(pure::toolchain_warning(true, short, full, "/e"), None);
        assert_eq!(pure::toolchain_warning(true, full, short, "/e"), None);
        assert!(pure::toolchain_warning(true, "v4.33.0", full, "/e").is_some());
    }

    /// ja: rc を、同じバージョンの正式リリースより前で、古いバージョンより後に並べる
    /// en: Orders an rc before the release of the same version and after older releases
    #[story("G3")]
    #[test]
    fn orders_an_rc_before_the_release_of_the_same_version_and_after_older_releases() {
        assert!(pure::version_key("v4.33.0-rc1") < pure::version_key("v4.33.0"));
        assert!(pure::version_key("v4.33.0") < pure::version_key("v4.34.0-rc1"));
    }

    /// ja: 解釈できないバージョンには None を返す
    /// en: Returns None for a version it cannot parse
    #[story("G3")]
    #[test]
    fn returns_none_for_a_version_it_cannot_parse() {
        for tag in ["v4.33", "nightly-2026-01-01", "4.33.0"] {
            assert_eq!(pure::version_key(tag), None, "解釈できてしまう: {tag:?}");
        }
    }

    /// ja: 同じ名前のタグがあれば、そのタグを返す
    /// en: Returns the tag with the same name if there is one
    #[story("G3")]
    #[test]
    fn returns_the_tag_with_the_same_name_if_there_is_one() {
        assert_eq!(pure::pick_tag("v4.33.0", &TAGS).as_deref(), Some("v4.33.0"));
    }

    /// ja: タグの無い patch リリースには、その前のタグを返す
    /// en: Returns the previous tag for a patch release without a tag
    #[story("G3")]
    #[test]
    fn returns_the_previous_tag_for_a_patch_release_without_a_tag() {
        // repl は v4.33.1 にタグを付けない。API は patch で変わらない。
        assert_eq!(pure::pick_tag("v4.33.1", &TAGS).as_deref(), Some("v4.33.0"));
    }

    /// ja: どのタグより古いバージョンか、解釈できないバージョンには、タグを返さない
    /// en: Returns no tag for a version older than every tag or one it cannot parse
    #[story("G3")]
    #[test]
    fn returns_no_tag_for_a_version_older_than_every_tag_or_one_it_cannot_parse() {
        assert_eq!(pure::pick_tag("v4.0.0", &["v4.32.0", "v4.33.0"]), None);
        assert_eq!(pure::pick_tag("nightly", &["v4.33.0"]), None);
    }

    /// ja: パスの指定が無ければ、キャッシュの下のバージョンの名前のパスを返す
    /// en: Returns a path named after the version under the cache when no path is given
    #[story("G3")]
    #[test]
    fn returns_a_path_named_after_the_version_under_the_cache_when_no_path_is_given() {
        let got = pure::engine_dir(None, "leanprover/lean4:v4.33.0", "/cache");
        assert_eq!(got.as_deref(), Ok("/cache/v4.33.0"));
    }

    /// ja: 指定されたパスはそのまま返す
    /// en: Returns a given path unchanged
    #[story("G3")]
    #[test]
    fn returns_a_given_path_unchanged() {
        let got = pure::engine_dir(Some("/opt/repl"), "leanprover/lean4:v4.33.0", "/cache");
        assert_eq!(got.as_deref(), Ok("/opt/repl"));
    }

    /// ja: .. や . のように、キャッシュの下の 1 つの場所を指さないバージョン名にはエラーを返す
    /// en: Returns an error for a version name such as .. or . that does not name one place in the cache
    #[story("G3")]
    #[test]
    fn returns_an_error_for_a_version_name_such_as_or_that_does_not_name_one_place_in_the_cache() {
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

/// 用意の失敗 (`Setup`) の理由。ほかの結果ならテストを失敗にする。
fn setup_why<T: std::fmt::Debug>(got: Result<T, EngineFault>) -> String {
    match got {
        Err(EngineFault::Setup(why)) => why,
        other => panic!("Setup にならない: {other:?}"),
    }
}

/// `mark` のファイルができたら、Ctrl-C が押されたことにする。偽のコマンドが
/// 動き始めてから止めるため。
fn interrupt_when(ctx: &Ctx, mark: &Path) -> thread::JoinHandle<()> {
    let flag = ctx.interrupted.clone();
    let mark = mark.to_path_buf();
    thread::spawn(move || {
        let end = Instant::now() + Duration::from_secs(30);
        while !mark.exists() && Instant::now() < end {
            thread::sleep(Duration::from_millis(10));
        }
        flag.store(true, SeqCst);
    })
}

/// 印のファイルを作ってから終わらずに待つ、偽のコマンドの中身。
fn hang_after(mark: &Path) -> String {
    format!(": > {}; sleep 30", path_str(mark))
}

mod types_of_config_values {
    //! ja: 設定の値の型
    //! en: Types of config values
    //!
    //! TOML には何でも書ける。型が違う値は panic ではなく ConfigError にする。
    use super::*;

    fn resolve_err(src: &str) -> String {
        config::resolve(&ctx_at("/"), None, None, &[], &table(src))
            .expect_err("エラーにならない")
            .0
    }

    /// ja: imports を文字列で書くと、配列を求めるエラーを返す
    /// en: Returns an error asking for an array when imports is a string
    #[story("G4")]
    #[test]
    fn returns_an_error_asking_for_an_array_when_imports_is_a_string() {
        // エラーにせず受け付けると 1 文字ずつの import になり、どれも解決できないので
        // ヘッダがすべて捨てられる (import Lean も消える)。
        let why = resolve_err("default = \"m\"\n[env.m]\nimports = \"Mathlib\"\n");
        assert!(why.contains("配列"), "{why}");
    }

    /// ja: 文字列で書くべき値が別の型なら、そのキーの名前を含むエラーを返す
    /// en: Returns an error naming the key when a value that should be a string has another type
    #[story("G4")]
    #[test]
    fn returns_an_error_naming_the_key_when_a_value_that_should_be_a_string_has_another_type() {
        for (key, value) in [("project", "3"), ("engine", "true"), ("prompt", "5")] {
            let why = resolve_err(&format!("default = \"m\"\n[env.m]\n{key} = {value}\n"));
            assert!(why.contains(key), "{why}");
        }
    }

    /// ja: 表で書くべき env や env.m が別の型なら、表を求めるエラーを返す
    /// en: Returns an error asking for a table when env or env.m is not a table
    #[story("G4")]
    #[test]
    fn returns_an_error_asking_for_a_table_when_env_or_env_m_is_not_a_table() {
        let why = resolve_err("default = \"m\"\nenv = 3\n");
        assert!(why.contains("env は表"), "{why}");
        let why = resolve_err("default = \"m\"\n[env]\nm = \"nope\"\n");
        assert!(why.contains("env.m は表"), "{why}");
    }

    /// ja: default が文字列でなければ、default を含むエラーを返す
    /// en: Returns an error naming default when default is not a string
    #[story("G4")]
    #[test]
    fn returns_an_error_naming_default_when_default_is_not_a_string() {
        let why = resolve_err("default = 3\n[env.m]\n");
        assert!(why.contains("default"), "{why}");
    }

    /// ja: 正しく書いた設定からは、名前と imports と prompt をそのまま返す
    /// en: Returns the name, imports and prompt unchanged from a correctly written config
    #[story("G4", "G5")]
    #[test]
    fn returns_the_name_imports_and_prompt_unchanged_from_a_correctly_written_config() {
        let cfg = table("default = \"m\"\n[env.m]\nimports = [\"A\", \"B\"]\nprompt = \"> \"\n");
        let got = config::resolve(&ctx_at("/"), None, None, &[], &cfg).expect("resolve");
        assert_eq!(
            (got.name.as_str(), got.imports, got.prompt.as_str()),
            ("m", vec!["A".to_string(), "B".to_string()], "> ")
        );
    }
}

mod choosing_the_environment_to_start {
    //! ja: 起動する環境の決め方
    //! en: Choosing the environment to start
    //!
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

    /// ja: 環境の名前を指定したら、default ではなくその環境を選ぶ
    /// en: Chooses the named environment over default when a name is given
    #[story("G5")]
    #[test]
    fn chooses_the_named_environment_over_default_when_a_name_is_given() {
        assert_eq!(resolve(&ctx_at("/"), Some("m"), None, &[]).name, "m");
    }

    /// ja: 名前と project と import を指定したら、環境の project と imports を置き換える
    /// en: Replaces the project and imports of the environment when a name, project and imports are given
    #[story("G5")]
    #[test]
    fn replaces_the_project_and_imports_of_the_environment_when_a_name_project_and_imports_are_given()
     {
        let got = resolve(&ctx_at("/"), Some("m"), Some("/p"), &["X"]);
        assert_eq!(
            (got.name.as_str(), got.imports, got.project.as_deref()),
            ("m", vec!["X".to_string()], Some("/p"))
        );
    }

    /// ja: import だけを指定したら、default でなく、その import の plain の環境を選ぶ
    /// en: Chooses the plain environment with the given imports when only imports are given
    #[story("G5", "G6")]
    #[test]
    fn chooses_the_plain_environment_with_the_given_imports_when_only_imports_are_given() {
        let dir = tmp();
        let got = resolve(&ctx_at(&path_str(dir.path())), None, None, &["X"]);
        assert_eq!(
            (got.name.as_str(), got.imports),
            ("plain", vec!["X".to_string()])
        );
    }

    /// ja: 環境に engine があればそれを使い、無ければ最上位の engine を使う
    /// en: Uses the engine of the environment if it has one and the top-level engine otherwise
    #[story("G3", "G5")]
    #[test]
    fn uses_the_engine_of_the_environment_if_it_has_one_and_the_top_level_engine_otherwise() {
        let ctx = ctx_at("/");
        let engine = |name| resolve(&ctx, name, None, &[]).engine;
        assert_eq!(engine(Some("m")).as_deref(), Some("/m/engine"));
        assert_eq!(engine(None).as_deref(), Some("/top/engine"));
    }

    /// ja: 設定に engine が無ければ LEANI_ENGINE の値を使う
    /// en: Uses LEANI_ENGINE when the config has no engine
    #[story("G3")]
    #[test]
    fn uses_leani_engine_when_the_config_has_no_engine() {
        let ctx = ctx_with(&[("LEANI_ENGINE", "/env/engine")], "/");
        let cfg = table("default = \"d\"\n[env.d]\n");
        let got = config::resolve(&ctx, None, None, &[], &cfg).expect("resolve");
        assert_eq!(got.engine.as_deref(), Some("/env/engine"));
    }

    /// ja: 名前を付けずに import だけを指定したら、最上位の engine を使う
    /// en: Uses the top-level engine when only imports are given without a name
    #[story("G3", "G5")]
    #[test]
    fn uses_the_top_level_engine_when_only_imports_are_given_without_a_name() {
        let dir = tmp();
        let got = resolve(&ctx_at(&path_str(dir.path())), None, None, &["X"]);
        assert_eq!(got.engine.as_deref(), Some("/top/engine"));
    }
}

mod reasons_to_abort_startup {
    //! ja: 起動を中止する理由
    //! en: Reasons to abort startup
    use super::*;

    /// elan、lake、git がそろった PATH の Ctx。
    fn ctx(dir: &Path) -> Ctx {
        ctx_with(&[("PATH", &tools_path(dir, &["elan", "lake", "git"]))], "/")
    }

    fn make(ctx: &Ctx, project: Option<&str>, engine: Option<&str>) -> EnvConfig {
        EnvConfig::make(ctx, "t", project, &[], None, engine)
    }

    /// ja: エンジンとプロジェクトのバージョンが違えば、そのことと lake build repl の手順を理由として返す
    /// en: Returns the version mismatch and the lake build repl step when engine and project versions differ
    #[story("G3", "G4")]
    #[test]
    fn returns_the_version_mismatch_and_the_lake_build_repl_step_when_engine_and_project_versions_differ()
     {
        let dir = tmp();
        let eng = built_engine(dir.path(), "leanprover/lean4:v4.33.0");
        let proj = toolchain_dir(dir.path(), "proj", "leanprover/lean4:v4.34.0");
        let ctx = ctx(dir.path());
        let why = config::problem(&ctx, &make(&ctx, Some(&proj), Some(&eng))).unwrap_or_default();
        assert!(why.contains("バージョンがプロジェクトと違う"), "{why}");
        assert!(why.contains("lake build repl"), "{why}");
    }

    /// ja: エンジンとプロジェクトの toolchain が書き方だけ違うなら、理由を返さない
    /// en: Returns no reason when engine and project toolchains differ only in how they are written
    #[story("G4")]
    #[test]
    fn returns_no_reason_when_engine_and_project_toolchains_differ_only_in_how_they_are_written() {
        let dir = tmp();
        let eng = built_engine(dir.path(), "leanprover/lean4:v4.34.0");
        let proj = toolchain_dir(dir.path(), "proj", "v4.34.0");
        let ctx = ctx(dir.path());
        let got = config::problem(&ctx, &make(&ctx, Some(&proj), Some(&eng)));
        assert_eq!(got, None);
    }

    /// ja: プロジェクトの場所が無ければ、Lake プロジェクトが無いという理由を返す
    /// en: Returns a reason saying there is no Lake project when the project directory does not exist
    #[story("G4")]
    #[test]
    fn returns_a_reason_saying_there_is_no_lake_project_when_the_project_directory_does_not_exist()
    {
        let dir = tmp();
        let eng = built_engine(dir.path(), "leanprover/lean4:v4.33.0");
        let no = path_str(&dir.path().join("no"));
        let ctx = ctx(dir.path());
        let why = config::problem(&ctx, &make(&ctx, Some(&no), Some(&eng))).unwrap_or_default();
        assert!(why.contains("Lake プロジェクトが無い"), "{why}");
    }

    /// ja: 指定したエンジンが無いか未ビルドなら、無いか未ビルドだという理由を返す
    /// en: Returns a reason saying the given engine is missing or not built when it is
    #[story("G3", "G4")]
    #[test]
    fn returns_a_reason_saying_the_given_engine_is_missing_or_not_built_when_it_is() {
        let dir = tmp();
        let ctx = ctx(dir.path());
        let no = path_str(&dir.path().join("no"));
        let why = config::problem(&ctx, &make(&ctx, None, Some(&no))).unwrap_or_default();
        assert!(why.contains("エンジンが無い"), "{why}");
        let eng = toolchain_dir(dir.path(), "eng", "leanprover/lean4:v4.33.0");
        let why = config::problem(&ctx, &make(&ctx, None, Some(&eng))).unwrap_or_default();
        assert!(why.contains("未ビルド"), "{why}");
    }

    /// ja: elan、lake、git のどれかが PATH に無ければ、無いコマンドの名前を含む理由を返す
    /// en: Returns a reason naming the missing command when elan, lake or git is not on PATH
    #[story("G4")]
    #[test]
    fn returns_a_reason_naming_the_missing_command_when_elan_lake_or_git_is_not_on_path() {
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

    /// ja: エンジン、プロジェクト、コマンドがそろっていれば、理由を返さない
    /// en: Returns no reason when the engine, project and commands are all in place
    #[story("G4")]
    #[test]
    fn returns_no_reason_when_the_engine_project_and_commands_are_all_in_place() {
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

mod loading_the_config_file {
    //! ja: 設定ファイルの読み込み
    //! en: Loading the config file
    use super::*;

    /// ja: TOML として読めない設定ファイルには、読めないという ConfigError を返す
    /// en: Returns a ConfigError saying the file cannot be read for a config file that is not valid TOML
    #[story("G4")]
    #[test]
    fn returns_a_configerror_saying_the_file_cannot_be_read_for_a_config_file_that_is_not_valid_toml()
     {
        let dir = tmp();
        let path = dir.path().join("config.toml");
        fs::write(&path, "default = \n").expect("write");
        let why = config::load_config(&path_str(&path)).expect_err("エラーにならない");
        assert!(why.0.contains("設定ファイルを読めない"), "{why}");
    }

    /// ja: 設定ファイルが無ければ空の設定を返す
    /// en: Returns an empty config when the config file does not exist
    #[story("G4", "G6")]
    #[test]
    fn returns_an_empty_config_when_the_config_file_does_not_exist() {
        let dir = tmp();
        let got = config::load_config(&path_str(&dir.path().join("none.toml")));
        assert_eq!(got, Ok(toml::Table::new()));
    }
}

mod choosing_the_version_to_use {
    //! ja: 使うバージョンの決め方
    //! en: Choosing the version to use
    //!
    //! elan run に渡すバージョン。
    //!
    //! エンジンをビルドしたバージョンと合わないと .olean を読み込めない。
    use super::*;

    /// ja: プロジェクトとエンジンの両方に toolchain があれば、プロジェクトの toolchain を返す
    /// en: Returns the project toolchain when both the project and the engine have one
    #[story("G3")]
    #[test]
    fn returns_the_project_toolchain_when_both_the_project_and_the_engine_have_one() {
        let dir = tmp();
        let ctx = ctx_at("/");
        let proj = toolchain_dir(dir.path(), "proj", "leanprover/lean4:v4.34.0-rc2");
        let eng = toolchain_dir(dir.path(), "eng", "leanprover/lean4:v4.33.0");
        let cfg = EnvConfig::make(&ctx, "t", Some(&proj), &[], None, Some(&eng));
        assert_eq!(
            boot::guess_toolchain(&ctx, &mut io::sink(), &cfg)
                .expect("guess")
                .as_deref(),
            Some("leanprover/lean4:v4.34.0-rc2")
        );
    }

    /// ja: プロジェクトが無く、エンジンを指定していれば、エンジンの toolchain を返す
    /// en: Returns the engine toolchain when an engine is given without a project
    #[story("G3")]
    #[test]
    fn returns_the_engine_toolchain_when_an_engine_is_given_without_a_project() {
        // 自分でビルドしたエンジンは leani がビルドし直さないので、そのバージョンに
        // 合わせるしかない。
        // elan のデフォルトのバージョンを使うと、4.34 の .olean を 4.33 で
        // 読み込むことになる。
        let dir = tmp();
        let ctx = ctx_at("/");
        let eng = toolchain_dir(dir.path(), "eng", "leanprover/lean4:v4.33.0");
        let cfg = EnvConfig::make(&ctx, "t", None, &[], None, Some(&eng));
        assert_eq!(
            boot::guess_toolchain(&ctx, &mut io::sink(), &cfg)
                .expect("guess")
                .as_deref(),
            Some("leanprover/lean4:v4.33.0")
        );
    }

    /// ja: lean のバージョンを調べている途中で Ctrl-C を押すと、中断を返す
    /// en: Returns an interrupt when Ctrl-C is pressed while checking the lean version
    #[story("G3")]
    #[test]
    fn returns_an_interrupt_when_ctrl_c_is_pressed_while_checking_the_lean_version() {
        let dir = tmp();
        let mark = dir.path().join("started");
        let path = fake_tools(dir.path(), &[("lean", &hang_after(&mark))]);
        let ctx = ctx_with(&[("PATH", &path)], "/");
        let cfg = EnvConfig::make(&ctx, "t", None, &[], None, None);
        let pressed = interrupt_when(&ctx, &mark);
        assert_eq!(
            boot::toolchain(&ctx, &mut io::sink(), &cfg),
            Err(EngineFault::Interrupted)
        );
        pressed.join().expect("join");
    }

    /// ja: プロジェクトもエンジンも無ければ、lean --version が示す elan の既定のバージョンを返す
    /// en: Returns the elan default version from lean --version when there is no project or engine
    #[story("G3")]
    #[test]
    fn returns_the_elan_default_version_from_lean_version_when_there_is_no_project_or_engine() {
        let dir = tmp();
        let lean = "echo 'Lean (version 4.9.0, x86_64-apple-darwin, commit abc, Release)'";
        let path = fake_tools(dir.path(), &[("lean", lean)]);
        let ctx = ctx_with(&[("PATH", &path)], "/");
        let cfg = EnvConfig::make(&ctx, "t", None, &[], None, None);
        assert_eq!(
            boot::guess_toolchain(&ctx, &mut io::sink(), &cfg)
                .expect("guess")
                .as_deref(),
            Some("leanprover/lean4:v4.9.0")
        );
    }

    /// `lean --version` が `out` を出す環境で、使うバージョンを決める。
    fn guess_with_lean(lean: &str, project_tc: Option<&str>) -> Option<String> {
        let dir = tmp();
        let path = fake_tools(dir.path(), &[("lean", lean)]);
        let ctx = ctx_with(&[("PATH", &path)], "/");
        let proj = project_tc.map(|tc| toolchain_dir(dir.path(), "proj", tc));
        let cfg = EnvConfig::make(&ctx, "t", proj.as_deref(), &[], None, None);
        boot::guess_toolchain(&ctx, &mut io::sink(), &cfg).expect("guess")
    }

    /// ja: lean --version が nightly のバージョンを示せば、nightly の toolchain を返す
    /// en: Returns a nightly toolchain when lean --version shows a nightly version
    #[story("G3")]
    #[test]
    fn returns_a_nightly_toolchain_when_lean_version_shows_a_nightly_version() {
        // v4.24.0 と読むと、別のバージョンのエンジンをビルドして .olean を読めなくなる。
        let lean = "echo 'Lean (version 4.24.0-nightly-2025-10-01, arm64-apple-darwin, commit abc, Release)'";
        assert_eq!(
            guess_with_lean(lean, None).as_deref(),
            Some("leanprover/lean4:nightly-2025-10-01")
        );
    }

    /// ja: lean --version が知らない接尾辞のバージョンを示せば、None を返す
    /// en: Returns None when lean --version shows a version with an unknown suffix
    #[story("G3")]
    #[test]
    fn returns_none_when_lean_version_shows_a_version_with_an_unknown_suffix() {
        let lean = "echo 'Lean (version 4.24.0-pre, arm64-apple-darwin, commit abc, Release)'";
        assert_eq!(guess_with_lean(lean, None), None);
    }

    /// ja: lean --version がエンジンを用意するときの時間の上限より長くかかっても、待ってそのバージョンの toolchain を返す
    /// en: Returns the toolchain of the version when lean --version takes longer than the setup timeout
    #[story("G3")]
    #[test]
    fn returns_the_toolchain_of_the_version_when_lean_version_takes_longer_than_the_setup_timeout()
    {
        // elan は lean --version の中で toolchain を取得することがある。途中で止めると、
        // 取得が終わる前に「バージョンが分からない」と表示する。
        let dir = tmp();
        let lean = "sleep 1; echo 'Lean (version 4.9.0, x86_64-apple-darwin, commit abc, Release)'";
        let path = fake_tools(dir.path(), &[("lean", lean)]);
        let mut ctx = ctx_with(&[("PATH", &path)], "/");
        ctx.setup_timeout = Duration::from_millis(100);
        let cfg = EnvConfig::make(&ctx, "t", None, &[], None, None);
        assert_eq!(
            boot::guess_toolchain(&ctx, &mut io::sink(), &cfg)
                .expect("guess")
                .as_deref(),
            Some("leanprover/lean4:v4.9.0")
        );
    }

    /// ja: lean のバージョンを調べるあいだ、端末にだけ表示し、終わったら消す
    /// en: Shows a message only on a terminal while checking the lean version and clears it after
    #[story("G3")]
    #[test]
    fn shows_a_message_only_on_a_terminal_while_checking_the_lean_version_and_clears_it_after() {
        // elan が toolchain を取得していると時間がかかる。何も表示しないと、leani が
        // 止まったように見える。終わったら表示を消す。
        let dir = tmp();
        let lean = "echo 'Lean (version 4.9.0, x86_64-apple-darwin, commit abc, Release)'";
        let path = fake_tools(dir.path(), &[("lean", lean)]);
        let mut ctx = ctx_with(&[("PATH", &path)], "/");
        let cfg = EnvConfig::make(&ctx, "t", None, &[], None, None);
        let shown = |ctx: &Ctx| {
            let mut out = Vec::new();
            boot::guess_toolchain(ctx, &mut out, &cfg).expect("guess");
            String::from_utf8(out).expect("utf-8")
        };
        assert_eq!(shown(&ctx), "");
        ctx.tty = true;
        let got = shown(&ctx);
        assert!(got.contains("Lean のバージョンを調べている"), "{got:?}");
        assert!(got.ends_with("\r\x1b[K"), "{got:?}");
    }

    /// ja: プロジェクトの toolchain が stable なら今のバージョンを返し、取得できなければ stable を返す
    /// en: Returns the current version for a stable project toolchain, or stable if it cannot get one
    #[story("G3")]
    #[test]
    fn returns_the_current_version_for_a_stable_project_toolchain_or_stable_if_it_cannot_get_one() {
        // チャンネルの名前のままだと、stable が新しいバージョンに進んでも、前の
        // バージョンでビルドしたエンジンを使い続ける。
        let lean = r#"case "$1" in
  +leanprover/lean4:stable) echo 'Lean (version 4.30.0, arm64-apple-darwin, commit abc, Release)' ;;
  *) exit 1 ;;
esac"#;
        assert_eq!(
            guess_with_lean(lean, Some("leanprover/lean4:stable")).as_deref(),
            Some("leanprover/lean4:v4.30.0")
        );
        // 今のバージョンを取得できなければ、チャンネルの名前のまま使う。
        assert_eq!(
            guess_with_lean("exit 1", Some("leanprover/lean4:stable")).as_deref(),
            Some("leanprover/lean4:stable")
        );
    }

    /// ja: 指定したエンジンの toolchain が stable なら、置き換えずに stable を返す
    /// en: Returns stable unchanged when a given engine has the stable toolchain
    #[story("G3")]
    #[test]
    fn returns_stable_unchanged_when_a_given_engine_has_the_stable_toolchain() {
        // 自分でビルドしたエンジンの lean-toolchain は stable のままなので、置き換えると
        // toolchain が違うという警告を起動のたびに表示する。
        let dir = tmp();
        let lean = "echo 'Lean (version 4.30.0, arm64-apple-darwin, commit abc, Release)'";
        let path = fake_tools(dir.path(), &[("lean", lean)]);
        let ctx = ctx_with(&[("PATH", &path)], "/");
        let eng = toolchain_dir(dir.path(), "eng", "leanprover/lean4:stable");
        let cfg = EnvConfig::make(&ctx, "t", None, &[], None, Some(&eng));
        assert_eq!(
            boot::guess_toolchain(&ctx, &mut io::sink(), &cfg)
                .expect("guess")
                .as_deref(),
            Some("leanprover/lean4:stable")
        );
    }
}

mod steps_to_set_up_the_engine_by_hand {
    //! ja: 手動でエンジンを用意する手順
    //! en: Steps to set up the engine by hand
    //!
    //! leani が自動で用意できなかったときに表示する。書いてあるとおりに実行すると、
    //! 自動で用意したものと同じものができる。
    use super::*;

    /// ja: タグが分かれば、そのタグの clone と lean-toolchain へのバージョンの書き込みを手順に含める
    /// en: Includes cloning the tag and writing the version to lean-toolchain if the tag is known
    #[story("G3")]
    #[test]
    fn includes_cloning_the_tag_and_writing_the_version_to_lean_toolchain_if_the_tag_is_known() {
        let out = boot::manual_setup("/x/e", "leanprover/lean4:v4.33.1", Some("v4.33.0"));
        assert!(out.contains("--branch v4.33.0"), "{out}");
        assert!(
            out.contains("leanprover/lean4:v4.33.1 > /x/e/lean-toolchain"),
            "{out}"
        );
    }

    /// ja: タグが分からなければ、clone するタグをバージョン以下で一番新しいタグと書く
    /// en: Describes the tag to clone as the newest one up to the version if the tag is unknown
    #[story("G3")]
    #[test]
    fn describes_the_tag_to_clone_as_the_newest_one_up_to_the_version_if_the_tag_is_unknown() {
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

mod safety_measures_in_setting_up_the_engine {
    //! ja: エンジンを用意するときの安全対策
    //! en: Safety measures in setting up the engine
    //!
    //! leani が削除してよいのは、leani 自身が作ったディレクトリだけ。
    //! leani を 2 つ同時に実行してもよい。
    use super::*;

    /// ja: ビルドしていないエンジンを指定すると、作り直さずに未ビルドだと知らせる
    /// en: Reports an unbuilt engine as unbuilt without rebuilding it if the user specifies it
    #[story("G3")]
    #[test]
    fn reports_an_unbuilt_engine_as_unbuilt_without_rebuilding_it_if_the_user_specifies_it() {
        // build_engine は置き場所を作り直すので、ユーザーが指定した場所に対して
        // 実行してはならない。git も lake も無い PATH なので、作ろうとすれば失敗する。
        let dir = tmp();
        let ctx = ctx_with(&[("PATH", &path_str(dir.path()))], "/");
        let eng = toolchain_dir(dir.path(), "eng", TC);
        let why = setup_why(boot::ensure_engine(
            &ctx,
            &mut sink(),
            Some(&eng),
            TC,
            false,
        ));
        assert!(why.contains("未ビルド"), "{why}");
        assert!(Path::new(&eng).join("lean-toolchain").is_file());
    }

    /// ja: lean-toolchain の無いエンジンを指定すると、lean-toolchain を含む中止の理由を返す
    /// en: Returns an abort reason mentioning lean-toolchain if the specified engine has none
    #[story("G3", "G4")]
    #[test]
    fn returns_an_abort_reason_mentioning_lean_toolchain_if_the_specified_engine_has_none() {
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

    /// ja: エンジンを用意すると、別の作業用のディレクトリでビルドしてから置き場所に移し、作業用のディレクトリを消す
    /// en: Builds in a work directory, moves the engine into place, and deletes the work directory on setup
    #[story("G3")]
    #[test]
    fn builds_in_a_work_directory_moves_the_engine_into_place_and_deletes_the_work_directory_on_setup()
     {
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

    /// ja: ビルドを終えたとき置き場所に別の leani のエンジンがあれば、それを残して作業用のディレクトリを消す
    /// en: Keeps the engine another leani placed and deletes the work directory when the build finishes
    #[story("G3")]
    #[test]
    fn keeps_the_engine_another_leani_placed_and_deletes_the_work_directory_when_the_build_finishes()
     {
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

mod when_the_engine_cannot_be_set_up_automatically {
    //! ja: エンジンを自動で用意できないとき
    //! en: When the engine cannot be set up automatically
    //!
    //! 初回の起動で通る処理。失敗しても、手動で用意する手順を表示する。
    use super::*;

    /// ja: repl のタグを取得できなければ、手動で用意する手順を含む中止の理由を返す
    /// en: Returns an abort reason with the manual setup steps if the repl tags cannot be fetched
    #[story("G3")]
    #[test]
    fn returns_an_abort_reason_with_the_manual_setup_steps_if_the_repl_tags_cannot_be_fetched() {
        // 終わらない、git が無い、失敗する、の 3 つ。exec で sh ごと sleep にするのは、
        // kill したあとに sleep だけが残ってパイプを閉じないため。
        let dir = tmp();
        let fails = [Some("exec sleep 5"), None, Some("echo fatal >&2; exit 1")];
        for fail in fails {
            let git = fail.map(|body| ("git", body));
            let path = fake_tools(dir.path(), &git.into_iter().collect::<Vec<_>>());
            let mut ctx = ctx_with(&[("PATH", &path)], "/");
            ctx.setup_timeout = Duration::from_millis(200);
            let why = setup_why(boot::ensure_engine(&ctx, &mut sink(), None, TC, false));
            assert!(why.contains("手動で用意する場合"), "{why}");
            assert!(why.contains("git clone --branch"), "{why}");
            let _ = fs::remove_file(dir.path().join("bin/git"));
        }
    }

    /// ja: タグの取得中に Ctrl-C を押すと、手動の手順を付けずに中断する
    /// en: Interrupts without the manual steps when Ctrl-C is pressed while fetching tags
    #[story("G3")]
    #[test]
    fn interrupts_without_the_manual_steps_when_ctrl_c_is_pressed_while_fetching_tags() {
        // 利用者が止めたので、手動で用意する手順は要らない。
        let dir = tmp();
        let mark = dir.path().join("started");
        let path = fake_tools(dir.path(), &[("git", &hang_after(&mark))]);
        let ctx = ctx_with(&[("PATH", &path)], "/");
        let pressed = interrupt_when(&ctx, &mark);
        let got = boot::ensure_engine(&ctx, &mut sink(), None, TC, false);
        pressed.join().expect("join");
        assert_eq!(got, Err(EngineFault::Interrupted));
    }

    /// ja: コマンドが孫プロセスを起動していても、Ctrl-C を押すと孫プロセスごと止めて中断する
    /// en: Stops a command with its grandchild process and interrupts when Ctrl-C is pressed
    #[story("G3")]
    #[test]
    fn stops_a_command_with_its_grandchild_process_and_interrupts_when_ctrl_c_is_pressed() {
        // 偽の git が起動した孫プロセス (sleep) の pid を記録しておき、止めたあとに
        // 残っていないことを確かめる。孫が残ると出力のパイプを閉じないので、
        // 出力をためる読み取りが孫の終了まで待ち続ける。
        // シェルはファイルを作ってから書くので、pid は別のファイルに書いてから
        // mv で置く。直接書くと、空のファイルを読むことがある。
        let dir = tmp();
        let pidfile = path_str(&dir.path().join("grandchild.pid"));
        let body =
            format!("sleep 30 & echo $! > {pidfile}.tmp && mv {pidfile}.tmp {pidfile}; wait");
        let path = fake_tools(dir.path(), &[("git", &body)]);
        let mut ctx = ctx_with(&[("PATH", &path)], "/");
        // 時間切れでは止めず、Ctrl-C だけで止める。止める処理は時間切れと同じ。
        ctx.setup_timeout = Duration::from_secs(60);
        let flag = ctx.interrupted.clone();
        let task = thread::spawn(move || boot::ensure_engine(&ctx, &mut sink(), None, TC, false));

        let end = Instant::now() + Duration::from_secs(30);
        let pid = loop {
            let read = fs::read_to_string(&pidfile).ok();
            if let Some(pid) = read.and_then(|t| t.trim().parse::<i32>().ok()) {
                break pid;
            }
            assert!(Instant::now() < end, "孫プロセスが起動しない");
            thread::sleep(Duration::from_millis(10));
        };
        let stopped = Instant::now();
        flag.store(true, SeqCst);
        let got = task.join().expect("join");

        assert_eq!(got, Err(EngineFault::Interrupted));
        assert!(
            stopped.elapsed() < Duration::from_secs(3),
            "孫プロセスが終わるまで待った"
        );
        assert!(
            gone(pid, Duration::from_secs(1)),
            "孫プロセス {pid} が残っている"
        );
    }

    /// pid のプロセスが `within` のうちに消えるか。
    fn gone(pid: i32, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            let alive = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok();
            if !alive {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// ja: タグの一覧の refs/tags/ の行から、タグの名前だけを返す
    /// en: Returns only the tag names from the refs/tags lines of the tag list
    #[story("G3")]
    #[test]
    fn returns_only_the_tag_names_from_the_refs_tags_lines_of_the_tag_list() {
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &["v4.33.0", "v4.34.0-rc1"], &[]);
        assert_eq!(
            boot::fetch_tags(&ctx).expect("fetch"),
            ["v4.33.0", "v4.34.0-rc1"]
        );
    }

    /// ja: toolchain をバージョンとして解釈できなければ、repl の master を clone する
    /// en: Clones the master branch of repl if the toolchain cannot be parsed as a version
    #[story("G3")]
    #[test]
    fn clones_the_master_branch_of_repl_if_the_toolchain_cannot_be_parsed_as_a_version() {
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &["v4.33.0"], &[]);
        boot::ensure_engine(&ctx, &mut sink(), None, "leanprover/lean4:nightly", false)
            .expect("ensure");
        let log = git_log(dir.path());
        assert!(log.contains("--branch master"), "{log}");
    }

    /// ja: repl にバージョンのタグが無ければ、clone せずにタグが無いことと手動の手順を知らせる
    /// en: Reports the missing tag and the manual steps without cloning if repl has no tag for the version
    #[story("G3", "G4")]
    #[test]
    fn reports_the_missing_tag_and_the_manual_steps_without_cloning_if_repl_has_no_tag_for_the_version()
     {
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &["v4.40.0"], &[]);
        let why = setup_why(boot::ensure_engine(&ctx, &mut sink(), None, TC, false));
        assert!(why.contains("repl に v4.33.0 用のタグが無い"), "{why}");
        assert!(why.contains("以下で一番新しいタグ"), "{why}");
        assert!(!git_log(dir.path()).contains("clone"), "ビルドしてしまった");
    }

    /// ja: LEANI_NO_SETUP があれば、--setup のときだけエンジンを用意する
    /// en: Sets up the engine only with --setup if LEANI_NO_SETUP is set
    #[story("G3")]
    #[test]
    fn sets_up_the_engine_only_with_setup_if_leani_no_setup_is_set() {
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &["v4.33.0"], &[("LEANI_NO_SETUP", "1")]);
        let why = setup_why(boot::ensure_engine(&ctx, &mut sink(), None, TC, false));
        assert!(why.contains("LEANI_NO_SETUP"), "{why}");
        assert_eq!(git_log(dir.path()), "", "git を呼んでしまった");

        // --setup で明示的に頼まれたときは用意する。
        let path = boot::ensure_engine(&ctx, &mut sink(), None, TC, true).expect("ensure");
        assert!(Path::new(&path).join(".lake/build/bin/repl").is_file());
    }

    /// ja: ビルドに失敗したら、選んだタグを書いた手動の手順を含む中止の理由を返す
    /// en: Returns an abort reason with manual steps naming the chosen tag if the build fails
    #[story("G3")]
    #[test]
    fn returns_an_abort_reason_with_manual_steps_naming_the_chosen_tag_if_the_build_fails() {
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &["v4.32.0"], &[]);
        let git = fake_git(&dir.path().join("git.log"), &["v4.32.0"]);
        fake_tools(dir.path(), &[("git", &git), ("lake", "exit 1")]);
        let why = setup_why(boot::ensure_engine(&ctx, &mut sink(), None, TC, false));
        assert!(why.contains("--branch v4.32.0"), "{why}");
    }
}

mod getting_lake_env {
    //! ja: lake env の取得
    //! en: Getting lake env
    //!
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

    /// ja: Lake プロジェクトが無ければ、lake env を実行せずに空の環境変数を返す
    /// en: Returns no variables without running lake env if there is no project
    #[story("G1")]
    #[test]
    fn returns_no_variables_without_running_lake_env_if_there_is_no_project() {
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &[], &[]);
        assert_eq!(
            boot::lake_env(&ctx, &mut sink(), None, TC),
            Ok(Default::default())
        );
        assert_eq!(lake_count(dir.path()), 0);
    }

    /// ja: エンジンの olean の場所を、lake env の LEAN_PATH か、プロジェクトが無ければ起動時の LEAN_PATH の先頭に足す
    /// en: Prepends the engine olean path to LEAN_PATH from lake env, or the inherited one without a project
    #[story("G3", "G6")]
    #[test]
    fn prepends_the_engine_olean_path_to_lean_path_from_lake_env_or_the_inherited_one_without_a_project()
     {
        // lake env に LEAN_PATH があればそれに、無ければ起動時の環境変数に足す。
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &[], &[("LEAN_PATH", "/inherited")]);
        let proj = project(dir.path());
        let lean_path = |project: Option<&str>| {
            let cfg = EnvConfig::make(&ctx, "t", project, &[], None, None);
            let env =
                leani::engine::proc_env(&ctx, &mut sink(), &cfg, TC, "/eng").expect("proc_env");
            env.get("LEAN_PATH").cloned().unwrap_or_default()
        };
        assert_eq!(lean_path(None), "/eng/.lake/build/lib/lean:/inherited");
        assert_eq!(lean_path(Some(&proj)), "/eng/.lake/build/lib/lean:/a");
    }

    /// ja: 同じプロジェクトで 2 回取得すると、2 回目は lake env を実行せずにキャッシュを返す
    /// en: Returns the cache without running lake env when getting it twice for the same project
    #[story("G1", "G6")]
    #[test]
    fn returns_the_cache_without_running_lake_env_when_getting_it_twice_for_the_same_project() {
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &[], &[]);
        let proj = project(dir.path());
        let want: std::collections::BTreeMap<_, _> =
            [("LEAN_PATH".to_string(), "/a".to_string())].into();
        let mut out = sink();
        assert_eq!(
            boot::lake_env(&ctx, &mut out, Some(&proj), TC),
            Ok(want.clone())
        );
        assert_eq!(boot::lake_env(&ctx, &mut out, Some(&proj), TC), Ok(want));
        assert_eq!(lake_count(dir.path()), 1);
    }

    /// ja: lake が無いか lake env が失敗したら、そのことを理由として知らせる
    /// en: Reports the reason if lake is missing or lake env fails
    #[story("G4", "G6")]
    #[test]
    fn reports_the_reason_if_lake_is_missing_or_lake_env_fails() {
        let dir = tmp();
        let proj = project(dir.path());
        let fails = [
            (None, "実行できなかった"),
            (Some("echo boom >&2; exit 1"), "失敗した"),
        ];
        for (fail, why) in fails {
            let lake = fail.map(|body| ("lake", body));
            let path = fake_tools(dir.path(), &lake.into_iter().collect::<Vec<_>>());
            let state = path_str(&dir.path().join("state"));
            let ctx = ctx_with(&[("PATH", &path), ("XDG_STATE_HOME", &state)], "/");
            let got = setup_why(boot::lake_env(&ctx, &mut sink(), Some(&proj), TC));
            assert!(got.starts_with("lake env "), "{got}");
            assert!(got.contains(why), "{got}");
            let _ = fs::remove_file(dir.path().join("bin/lake"));
        }
    }

    /// ja: lake env を実行すると、Ctrl-C で中断できると表示する
    /// en: Shows that Ctrl-C can interrupt lake env when it runs
    #[story("G6")]
    #[test]
    fn shows_that_ctrl_c_can_interrupt_lake_env_when_it_runs() {
        // 初回の lake env は依存パッケージを取得することがあり、何も表示しないと
        // 止まったように見える。
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &[], &[]);
        let proj = project(dir.path());
        let mut out = sink();
        boot::lake_env(&ctx, &mut out, Some(&proj), TC).expect("lake env");
        let shown = String::from_utf8_lossy(&out);
        assert!(shown.contains("Ctrl-C で中断できる"), "{shown}");
    }

    /// ja: lake env がエンジンを用意するときの時間の上限より長くかかっても、待って結果を返す
    /// en: Returns the result of lake env when it takes longer than the setup timeout
    #[story("G6")]
    #[test]
    fn returns_the_result_of_lake_env_when_it_takes_longer_than_the_setup_timeout() {
        // 依存パッケージの取得は、外部との通信のタイムアウトより長くかかることがある。
        let dir = tmp();
        let proj = project(dir.path());
        let lake = "sleep 1; printf 'LEAN_PATH=/a\\n'";
        let path = fake_tools(dir.path(), &[("lake", lake)]);
        let state = path_str(&dir.path().join("state"));
        let mut ctx = ctx_with(&[("PATH", &path), ("XDG_STATE_HOME", &state)], "/");
        ctx.setup_timeout = Duration::from_millis(200);
        let got = boot::lake_env(&ctx, &mut sink(), Some(&proj), TC).expect("lake env");
        assert_eq!(got.get("LEAN_PATH").map(String::as_str), Some("/a"));
    }

    /// ja: lake env の実行中に Ctrl-C を押すと、中断する
    /// en: Interrupts when Ctrl-C is pressed while lake env runs
    #[story("G6")]
    #[test]
    fn interrupts_when_ctrl_c_is_pressed_while_lake_env_runs() {
        let dir = tmp();
        let proj = project(dir.path());
        let mark = dir.path().join("started");
        let path = fake_tools(dir.path(), &[("lake", &hang_after(&mark))]);
        let state = path_str(&dir.path().join("state"));
        let ctx = ctx_with(&[("PATH", &path), ("XDG_STATE_HOME", &state)], "/");
        let pressed = interrupt_when(&ctx, &mark);
        let got = boot::lake_env(&ctx, &mut sink(), Some(&proj), TC);
        pressed.join().expect("join");
        assert_eq!(got, Err(EngineFault::Interrupted));
    }

    /// ja: lake env の結果を保存できなければ、保存できなかったと知らせて一時ファイルを残さない
    /// en: Reports the failure and leaves no temp file if the lake env result cannot be saved
    #[story("G6")]
    #[test]
    fn reports_the_failure_and_leaves_no_temp_file_if_the_lake_env_result_cannot_be_saved() {
        // キャッシュの置き場所にディレクトリがあると rename できない。
        let dir = tmp();
        let ctx = setup_ctx(dir.path(), &[], &[]);
        let proj = project(dir.path());
        let key = sha1_smol::Sha1::from(format!("{proj}\n{TC}"))
            .digest()
            .to_string();
        let cache = format!("{}/lake-env/proj-{}", ctx.state, &key[..12]);
        fs::create_dir_all(&cache).expect("mkdir");

        let got = setup_why(boot::lake_env(&ctx, &mut sink(), Some(&proj), TC));
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

mod recording_tactics_in_proof_mode {
    //! ja: 証明モードのタクティクの記録
    //! en: Recording tactics in proof mode
    use super::*;

    fn start() -> Proof {
        Proof::new(0, vec!["⊢ p ∧ q".to_string()], 1)
    }

    fn strings(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }

    /// ja: タクティクを進めると、前の状態とゴールを積み、スクリプトにタクティクを足す
    /// en: Pushes the previous state and goals and adds the tactic to the script when a tactic is applied
    #[story("E1")]
    #[test]
    fn pushes_the_previous_state_and_goals_and_adds_the_tactic_to_the_script_when_a_tactic_is_applied()
     {
        let got = pure::tactic_step(&start(), 3, &strings(&["⊢ p", "⊢ q"]), "constructor");
        assert_eq!(
            (got.state, &got.goals, got.generation),
            (3, &strings(&["⊢ p", "⊢ q"]), 1)
        );
        assert_eq!(got.script, strings(&["constructor"]));
        assert_eq!(got.stack, [(0, strings(&["⊢ p ∧ q"]))]);
    }

    /// ja: 取り消すと状態とゴールとスクリプトを前に戻し、戻す先が無ければ None を返す
    /// en: Restores the state, goals, and script when undone, and returns None with nothing to undo
    #[story("E1")]
    #[test]
    fn restores_the_state_goals_and_script_when_undone_and_returns_none_with_nothing_to_undo() {
        let step = pure::tactic_step(&start(), 3, &strings(&["⊢ p", "⊢ q"]), "constructor");
        assert_eq!(pure::tactic_undo(&step), Some(start()));
        assert_eq!(pure::tactic_undo(&start()), None);
    }
}

mod tactics_sent_in_proof_mode {
    //! ja: 証明モードで送るタクティク
    //! en: Tactics sent in proof mode
    use super::*;

    /// ja: タクティクを括弧で囲み、括弧はそれぞれ別の行に置く
    /// en: Wraps the tactic in parentheses placed on their own lines
    #[story("E1")]
    #[test]
    fn wraps_the_tactic_in_parentheses_placed_on_their_own_lines() {
        assert_eq!(
            pure::grouped_tactic("constructor; rfl -- c"),
            "(\nconstructor; rfl -- c\n)"
        );
    }

    /// ja: 完結判定にエラーがあれば、パースエラーを完結判定で読んだ位置とメッセージにする
    /// en: Takes the parse error position and message from the completeness check if it has an error
    #[story("E1")]
    #[test]
    fn takes_the_parse_error_position_and_message_from_the_completeness_check_if_it_has_an_error() {
        let probe = json!({"tac": {"ok": false, "err": "<input>:2:6: expected end of input"}});
        assert_eq!(
            pure::tactic_parse_error("Lean error:\n<input>:1:4: expected ')'", Some(&probe)),
            "Lean error:\n<input>:1:4: expected end of input"
        );
    }

    /// ja: 完結判定にエラーが無ければ、パースエラーの行を括弧の行の分だけ戻す
    /// en: Moves the parse error line back by the parenthesis line if the completeness check has no error
    #[story("E1")]
    #[test]
    fn moves_the_parse_error_line_back_by_the_parenthesis_line_if_the_completeness_check_has_no_error()
     {
        assert_eq!(
            pure::tactic_parse_error("Lean error:\n<input>:2:17: expected ')'", None),
            "Lean error:\n<input>:1:17: expected ')'"
        );
        let ok = json!({"tac": {"ok": true}});
        assert_eq!(
            pure::tactic_parse_error("Unknown proof state", Some(&ok)),
            "Unknown proof state"
        );
    }
}

mod replacing_sorry {
    //! ja: sorry の置き換え
    //! en: Replacing sorry
    //!
    //! repl の pos / endPos で切り出してスクリプトに置き換える。桁はコードポイント
    //! 単位で、`pure::offset` が byte の位置に直す。
    use super::*;

    fn splice(src: &str, word: &str, script: &str) -> Option<String> {
        pure::splice_sorry(src, &mark(src, word, 0), script)
    }

    /// ja: sorry の前に日本語があっても、位置どおりに sorry を置き換える
    /// en: Replaces sorry at its position with Japanese text before it
    #[story("E3")]
    #[test]
    fn replaces_sorry_at_its_position_with_japanese_text_before_it() {
        let src = "theorem a : True := by /- あいう -/ sorry";
        assert_eq!(
            splice(src, "sorry", "trivial").as_deref(),
            Some("theorem a : True := by /- あいう -/ trivial")
        );
    }

    /// ja: 単独の行にある sorry は、インデントを変えずに置き換える
    /// en: Replaces sorry on its own line without changing the indentation
    #[story("E3")]
    #[test]
    fn replaces_sorry_on_its_own_line_without_changing_the_indentation() {
        // 空白を詰めるとインデントが浅くなり、内側の by ブロックの外に出てしまう。
        let src = "example : True := by\n  have h : True := by\n    sorry\n  exact h";
        assert_eq!(
            splice(src, "sorry", "trivial").as_deref(),
            Some("example : True := by\n  have h : True := by\n    trivial\n  exact h")
        );
    }

    /// ja: 複数行のスクリプトは、各行を sorry の桁にそろえる
    /// en: Aligns every line of a multi-line script to the sorry column
    #[story("E3")]
    #[test]
    fn aligns_every_line_of_a_multi_line_script_to_the_sorry_column() {
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

    /// ja: 行の途中の sorry を複数行のスクリプトにすると、スクリプトを by の次の行に下げる
    /// en: Moves a multi-line script to the line after by when it replaces a sorry in the middle of a line
    #[story("E3")]
    #[test]
    fn moves_a_multi_line_script_to_the_line_after_by_when_it_replaces_a_sorry_in_the_middle_of_a_line()
     {
        let src = "theorem t (n : Nat) : n + 0 = n := by sorry";
        assert_eq!(
            splice(src, "sorry", "induction n with\n| zero => rfl").as_deref(),
            Some("theorem t (n : Nat) : n + 0 = n := by\n  induction n with\n  | zero => rfl")
        );
    }

    /// ja: 匿名コンストラクタの中の sorry は、空白を足さずに置き換える
    /// en: Replaces sorry inside an anonymous constructor without adding spaces
    #[story("E3")]
    #[test]
    fn replaces_sorry_inside_an_anonymous_constructor_without_adding_spaces() {
        let src = "example : True ∧ True := ⟨by sorry, by trivial⟩";
        assert_eq!(
            splice(src, "sorry", "trivial").as_deref(),
            Some("example : True ∧ True := ⟨by trivial, by trivial⟩")
        );
    }

    /// ja: 複数行の文字列を含むスクリプトでは、文字列の外の行だけにインデントを付ける
    /// en: Indents only the lines outside strings in a script with a multi-line string
    #[story("E3")]
    #[test]
    fn indents_only_the_lines_outside_strings_in_a_script_with_a_multi_line_string() {
        // 文字列の 2 行目に空白を足すと、文字列の値が変わる。
        let src = "def s : String := by\n  sorry";
        assert_eq!(
            splice(src, "sorry", "exact \"a\nb\"\n-- c").as_deref(),
            Some("def s : String := by\n  exact \"a\nb\"\n  -- c")
        );
        let src = "def s : String := by sorry";
        assert_eq!(
            splice(src, "sorry", "exact \"a\nb\"").as_deref(),
            Some("def s : String := by\n  exact \"a\nb\"")
        );
    }

    /// ja: 文字列の中で始まる行を除いて、各行にインデントを付ける
    /// en: Indents every line except those that start inside a string
    #[story("A1", "E3")]
    #[test]
    fn indents_every_line_except_those_that_start_inside_a_string() {
        let src = "f \"a\\\"\nb\" r#\"c\n\"d\"#\nx";
        assert_eq!(pure::outside_strings(src), [true, false, false, true]);
        assert_eq!(
            pure::indent_code(src, "  "),
            "  f \"a\\\"\nb\" r#\"c\n\"d\"#\n  x"
        );
    }

    /// ja: コメントの中の引用符のあとの行を、文字列の外と判定する
    /// en: Judges the line after a quote in a comment as outside strings
    #[story("A1", "E3")]
    #[test]
    fn judges_the_line_after_a_quote_in_a_comment_as_outside_strings() {
        let src = "/- \" -/ -- \"\nx";
        assert_eq!(pure::outside_strings(src), [true, true]);
    }

    /// ja: 文字のリテラルと名前のエスケープの中の引用符のあとの行を、文字列の外と判定する
    /// en: Judges the line after a quote in a char literal or a name escape as outside strings
    #[story("A1", "E3")]
    #[test]
    fn judges_the_line_after_a_quote_in_a_char_literal_or_a_name_escape_as_outside_strings() {
        let src = "'\"' «x\"»\nx";
        assert_eq!(pure::outside_strings(src), [true, true]);
    }

    /// ja: 補間の中の文字列が閉じても、外側の文字列はまだ閉じていないと判定する
    /// en: Judges the outer string as still open after a string in an interpolation closes
    #[story("A1", "E3")]
    #[test]
    fn judges_the_outer_string_as_still_open_after_a_string_in_an_interpolation_closes() {
        let src = "s!\"{\"e\nf\"}\ng\"\nx";
        assert_eq!(pure::outside_strings(src), [true, false, false, true]);
    }

    /// ja: 行の中で閉じない名前のエスケープは、行末で終わったと判定する
    /// en: Judges a name escape that does not close on its line as ended at the line end
    #[story("A1", "E3")]
    #[test]
    fn judges_a_name_escape_that_does_not_close_on_its_line_as_ended_at_the_line_end() {
        let src = "«y\n\"a\nb\"";
        assert_eq!(pure::outside_strings(src), [true, true, false]);
    }

    /// ja: どの入力でも、行の数と同じ数の判定を返す
    /// en: Returns one judgment per line for any input
    #[story("A1", "E3")]
    #[proptest]
    fn returns_one_judgment_per_line_for_any_input(
        #[strategy("[a\"'r#«»{}!/\\\\\\- \n]{0,40}")] src: String,
    ) {
        // `outside_strings` の返す値が行の数より少ないと、`indent_code` の出力から
        // 後ろの行が消える。
        prop_assert_eq!(pure::outside_strings(&src).len(), src.split('\n').count());
        prop_assert_eq!(pure::indent_code(&src, ""), src);
    }

    /// ja: 位置が sorry を指していなければ、置き換えずに None を返す
    /// en: Returns None without replacing if the position does not point at sorry
    #[story("E3")]
    #[test]
    fn returns_none_without_replacing_if_the_position_does_not_point_at_sorry() {
        let src = "example : True := by trivial";
        // 位置は合っているが指しているのは sorry ではない / 行が無い。
        assert_eq!(splice(src, "trivial", "rfl"), None);
        let out: Json = serde_json::from_str(POS_OUT).expect("POS_OUT");
        assert_eq!(pure::splice_sorry(src, &out, "rfl"), None);
        let none = json!({"pos": null, "endPos": null});
        assert_eq!(pure::splice_sorry(src, &none, "trivial"), None);
    }

    /// ja: どの行と桁でも、その文字の byte の位置を返す
    /// en: Returns the byte offset of the character for any line and column
    #[story("E3")]
    #[proptest]
    fn returns_the_byte_offset_of_the_character_for_any_line_and_column(
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

    /// ja: sorry の前後がどんな行でも、前後を書き換えずに sorry だけを置き換える
    /// en: Replaces only sorry and leaves the text around it unchanged for any surrounding lines
    #[story("E3")]
    #[proptest]
    fn replaces_only_sorry_and_leaves_the_text_around_it_unchanged_for_any_surrounding_lines(
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

mod imports_of_a_file {
    //! ja: ファイルの import
    //! en: Imports of a file
    //!
    //! :save のヘッダに書き戻すため、読み込んだファイルの import を記録する。
    use super::*;

    /// ja: import の行から module の名前だけを返す
    /// en: Returns only the module names from the import lines
    #[story("B3", "B4")]
    #[test]
    fn returns_only_the_module_names_from_the_import_lines() {
        let src = "import Lean.Elab\nimport  Std\n\ndef a := 1\n-- import Nope\n";
        assert_eq!(pure::import_lines(src), ["Lean.Elab", "Std"]);
    }

    /// ja: 宣言と、module の名前の無い import しか無ければ、空を返す
    /// en: Returns nothing for a declaration and an import without a module name
    #[story("B3")]
    #[test]
    fn returns_nothing_for_a_declaration_and_an_import_without_a_module_name() {
        assert!(pure::import_lines("def a := 1\nimport\n").is_empty());
    }

    /// ja: コメントの中の import は数えず、import を取り除くときも残す
    /// en: Skips imports in comments and keeps them when stripping imports
    #[story("B3", "B4")]
    #[test]
    fn skips_imports_in_comments_and_keeps_them_when_stripping_imports() {
        // ヘッダに書くと repl はヘッダ全体を捨てて起動するので、書き出したファイルは
        // :l でも lean でもエラーになる。import 行を取り除く処理も doc コメントの
        // 中身を変える。
        let src = "/-\nimport Nope.NotAModule\n-/\n-- import Also.Not\nimport Std\n";
        assert_eq!(pure::import_lines(src), ["Std"]);
        assert!(pure::strip_imports(src).contains("import Nope.NotAModule"));
        assert!(!pure::strip_imports(src).contains("import Std"));
    }

    /// ja: 宣言のあとの import しか無ければ、空を返す
    /// en: Returns nothing for an import after a declaration
    #[story("B3")]
    #[test]
    fn returns_nothing_for_an_import_after_a_declaration() {
        // Lean も先頭以外の import を認めない。数えると :save のヘッダが増える。
        assert!(pure::import_lines("def a := 1\nimport Std\n").is_empty());
    }

    /// ja: 入れ子のコメントが閉じたあとの import だけを返す
    /// en: Returns only the import after a nested comment closes
    #[story("B4")]
    #[test]
    fn returns_only_the_import_after_a_nested_comment_closes() {
        let src = "/- /- import In.Nest -/ import Still.In -/\nimport Real\n";
        assert_eq!(pure::import_lines(src), ["Real"]);
    }

    /// ja: 行の中にブロックコメントがあっても、import の module の名前を返す
    /// en: Returns the module names of imports with block comments on the same line
    #[story("B4")]
    #[test]
    fn returns_the_module_names_of_imports_with_block_comments_on_the_same_line() {
        let src = "import A /- c -/\n/- c -/ import B\n/- c -/\nimport C -- m\ndef x := 1";
        assert_eq!(pure::import_lines(src), ["A", "B", "C"]);
        assert_eq!(pure::strip_imports(src).trim(), "/- c -/\ndef x := 1");
    }

    /// ja: 名前のエスケープの中に -- や /- があっても、その名前を返す
    /// en: Returns names in name escapes that contain -- or /-
    #[story("B4")]
    #[test]
    fn returns_names_in_name_escapes_that_contain_or() {
        let src = "import «A--B»\nimport «C/-D» -- m\ndef x := 1";
        assert_eq!(pure::import_lines(src), ["«A--B»", "«C/-D»"]);
        assert_eq!(pure::strip_imports(src).trim(), "def x := 1");
    }

    /// ja: public や meta や all の付いた import から module の名前を返す
    /// en: Returns the module names of imports with public, meta, or all
    #[story("B4")]
    #[test]
    fn returns_the_module_names_of_imports_with_public_meta_or_all() {
        let src = "module\n\npublic import Lean.Elab\nmeta import all Std\nimport all Foo\n\
                   public meta import Bar\ndef x := 1";
        assert_eq!(pure::import_lines(src), ["Lean.Elab", "Std", "Foo", "Bar"]);
        assert_eq!(
            pure::scan_header(src).0.imports,
            [
                "public import Lean.Elab",
                "meta import all Std",
                "import all Foo",
                "public meta import Bar"
            ]
        );
        // module の行も取り除く。既存の環境の上では module を宣言できない。
        assert_eq!(pure::strip_imports(src).trim(), "def x := 1");
    }

    /// ja: コメントの付いた module の行を module の宣言と判定する
    /// en: Judges a module line with a comment as a module declaration
    #[story("B4")]
    #[test]
    fn judges_a_module_line_with_a_comment_as_a_module_declaration() {
        let src = "module -- メモ\nimport Std\ndef x := 1";
        assert!(pure::scan_header(src).0.module);
        assert_eq!(pure::strip_imports(src).trim(), "def x := 1");
        assert!(!pure::scan_header("modules\ndef x := 1").0.module);
    }

    /// ja: ファイルが module なら、足す import を module の行の後ろに public で置く
    /// en: Puts the added import after the module line as public in a module file
    #[story("B4")]
    #[test]
    fn puts_the_added_import_after_the_module_line_as_public_in_a_module_file() {
        let probe = "import Lean\n";
        let cfg = "import Foo\n";
        let got = pure::with_header("-- メモ\nmodule\nimport Std\ndef a := 1", probe, cfg);
        assert!(
            got.starts_with("module\npublic import Lean\nimport Std\n"),
            "{got}"
        );
        assert!(!got.contains("import Foo"), "{got}");
        assert_eq!(got.matches("module").count(), 1, "{got}");
        assert!(
            got.contains("-- メモ") && got.ends_with("def a := 1"),
            "{got}"
        );

        // module でなければ、今までどおり先頭に足す。ファイルに import があれば、
        // 設定の import は足さない。
        assert_eq!(
            pure::with_header("def a := 1", probe, cfg),
            "import Lean\nimport Foo\ndef a := 1"
        );
        assert_eq!(
            pure::with_header("import Std\ndef a := 1", probe, cfg),
            "import Lean\nimport Std\ndef a := 1"
        );
    }

    /// ja: どのヘッダでも、本物の import 行だけを数え、取り除くと import が残らない
    /// en: Counts only real import lines in any header and leaves no imports after stripping
    #[story("B3", "B4")]
    #[proptest]
    fn counts_only_real_import_lines_in_any_header_and_leaves_no_imports_after_stripping(
        #[strategy(prop::collection::vec(prop::sample::select(HEADERS.to_vec()), 0..=12))]
        lines: Vec<&'static str>,
    ) {
        let src = lines.join("\n");
        let (head, at) = pure::scan_header(&src);
        let stripped = pure::strip_imports(&src);

        let imports = at.iter().filter(|&&n| lines[n].trim() != "module");
        prop_assert_eq!(head.imports.len(), imports.count());
        // コメントの中の import を取り出していない。取り出すと :save した
        // ファイルがおかしくなる。
        let header_line = |n: &usize| {
            let first = lines[*n].split_whitespace().next();
            matches!(first, Some("import" | "public" | "meta" | "module"))
        };
        prop_assert!(at.iter().all(header_line));
        // 取り除いたあとに新しい import 行が現れることはない。ヘッダの終わりは
        // 変わらないので、1 回ですべて取り除ける。
        prop_assert!(pure::import_lines(&stripped).is_empty());
    }
}

mod history_file_format {
    //! ja: 履歴ファイルの形式
    //! en: History file format
    //!
    //! prompt_toolkit の形式の履歴ファイルを読む純粋関数。
    use super::*;
    use leani_core::history::history_strings;

    /// ja: CRLF で書かれた履歴からは、行末の CR を外した入力を返す
    /// en: Returns inputs without the trailing CR from a history file with CRLF
    #[story("F3")]
    #[test]
    fn returns_inputs_without_the_trailing_cr_from_a_history_file_with_crlf() {
        // CRLF で書かれた履歴をそのまま読むと、呼び出した入力の各行の末尾に \r が
        // 付いたままエンジンに送られる。
        let text = "\r\n# 2026-10-26 12:34:56\r\n+def f : Nat -> Nat\r\n+  | 0 => 1\r\n";
        assert_eq!(history_strings(text), ["def f : Nat -> Nat\n  | 0 => 1"]);
    }
}

mod expanding_abbreviations {
    //! ja: 略記の展開
    //! en: Expanding abbreviations
    //!
    //! `\name` を Lean の記号に変換する。変換は space か Tab で確定し、キー入力の
    //! たびには確認しない。
    use super::*;

    /// ja: 末尾の略記の名前を記号に変換する
    /// en: Converts the abbreviation name at the end to a symbol
    #[story("C5")]
    #[test]
    fn converts_the_abbreviation_name_at_the_end_to_a_symbol() {
        assert_eq!(expand_abbrev("a \\to"), Some(("→", 3)));
        assert_eq!(expand_abbrev("x\\dot"), Some(("·", 4)));
    }

    /// ja: 短い略記で始まる長い略記は、長い略記として変換する
    /// en: Converts a long abbreviation that starts with a shorter one as the long one
    #[story("C5")]
    #[test]
    fn converts_a_long_abbreviation_that_starts_with_a_shorter_one_as_the_long_one() {
        // `\a` も `\all` も `\alpha` も表にある。キー入力のたびに確定すると `\a` の
        // 時点で変換されてしまい、長いほうを入力できなくなる。
        assert_eq!(expand_abbrev("\\a"), Some(("α", 2)));
        assert_eq!(expand_abbrev("\\all"), Some(("∀", 4)));
        assert_eq!(expand_abbrev("\\alpha"), Some(("α", 6)));
    }

    /// ja: めったに使わない略記を変換する
    /// en: Converts rarely used abbreviations
    #[story("C5")]
    #[test]
    fn converts_rarely_used_abbreviations() {
        // 表には VS Code の Lean 拡張の略記をほぼすべて (1829 件) 含めている。
        assert_eq!(expand_abbrev("\\frown"), Some(("⌢", 6)));
        assert_eq!(expand_abbrev("\\Gangia"), Some(("Ϫ", 7)));
    }

    /// ja: 表に無い名前と、略記の無い入力には None を返す
    /// en: Returns None for a name not in the table or input without an abbreviation
    #[story("C5")]
    #[test]
    fn returns_none_for_a_name_not_in_the_table_or_input_without_an_abbreviation() {
        assert_eq!(expand_abbrev("\\nosuch"), None);
        assert_eq!(expand_abbrev("1 + 1"), None);
    }

    /// ja: 略記の名前のあとに記号が続けば、None を返す
    /// en: Returns None if a symbol follows the abbreviation name
    #[story("C5")]
    #[test]
    fn returns_none_if_a_symbol_follows_the_abbreviation_name() {
        // `\to)` 全体で表を検索する。`"\t"` のような文字列を ▸ に変換しない。
        assert_eq!(expand_abbrev("(\\to)"), None);
        assert_eq!(expand_abbrev("\"\\t\""), None);
    }

    /// ja: 表のどの略記でも、手前に何があっても記号に変換する
    /// en: Converts every abbreviation in the table whatever precedes it
    #[story("C5")]
    #[proptest]
    fn converts_every_abbreviation_in_the_table_whatever_precedes_it(
        #[strategy(abbrev_keys())] key: &'static str,
        #[strategy("[^\\\\]{0,8}")] head: String,
    ) {
        // 例に書けるのは数件。1829 件すべてが同じ規則で変換できることはここで確認する。
        // 入力位置の手前に何が書いてあっても、直前の `\` より後ろだけをキーにする。
        let want = (ABBREV[key].as_str(), key.len() + 1);
        prop_assert_eq!(expand_abbrev(&format!("{head}\\{key}")), Some(want));
    }
}

mod abbreviation_candidates {
    //! ja: 略記の候補
    //! en: Abbreviation candidates
    //!
    //! Tab で略記を補完するときに、カーソルの手前の `\name` から記号の候補を作る。
    use super::*;

    /// ja: カーソルの手前の略記の名前を返す
    /// en: Returns the abbreviation name before the cursor
    #[story("C8", "J2")]
    #[test]
    fn returns_the_abbreviation_name_before_the_cursor() {
        assert_eq!(abbrev_head("theorem x : p \\to"), Some("to"));
        assert_eq!(abbrev_head("(\\."), Some("."));
        assert_eq!(abbrev_head("\\"), Some(""));
    }

    /// ja: 略記の途中でなければ None を返す
    /// en: Returns None outside an abbreviation
    #[story("C8", "J2")]
    #[test]
    fn returns_none_outside_an_abbreviation() {
        assert_eq!(abbrev_head("Nat.suc"), None);
        assert_eq!(abbrev_head("\\to x"), None, "空白のあとは略記ではない");
    }

    /// ja: 文字列のエスケープのあとの名前は、略記ではなく名前を補完の対象として返す
    /// en: Returns a name, not an abbreviation, as the completion target after an escape in a string
    #[story("D1")]
    #[test]
    fn returns_a_name_not_an_abbreviation_as_the_completion_target_after_an_escape_in_a_string() {
        // `\n",Nat.su` で始まる略記は無いので、`\` から後ろは略記ではない。
        let text = r#"#eval ("a\n",Nat.su"#;
        assert_eq!(
            pure::completion_target(text),
            Target::Name {
                start: text.len() - "Nat.su".len(),
                prefix: "Nat.su".to_string()
            }
        );
    }

    /// ja: 名前と一致する略記の記号を先頭に置き、名前で始まる略記の記号を含める
    /// en: Puts the exact match symbol first and includes those of abbreviations starting with the name
    #[story("C8", "J2")]
    #[test]
    fn puts_the_exact_match_symbol_first_and_includes_those_of_abbreviations_starting_with_the_name()
     {
        // `\a` は α になる。`\all` の ∀ などはそのあとに続く。
        let got = abbrev_candidates("a");
        assert_eq!(got.first(), Some(&"α"), "{got:?}");
        assert!(got.contains(&"∀"), "{got:?}");
    }
}

mod theorem_search {
    //! ja: 定理検索
    //! en: Theorem search
    //!
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

    /// ja: 件数と、名前と型と、定義されている module を表示する
    /// en: Shows the count, the name with its type, and the defining module
    #[story("D4")]
    #[test]
    fn shows_the_count_the_name_with_its_type_and_the_defining_module() {
        let out = text(&json!({"count": 2, "hits": hits()}));
        let out: Vec<&str> = out.split('\n').collect();
        assert_eq!(out[0], "2 件");
        // `type` は先頭に空白が付いているので、`name : type` の形に整形する。
        assert_eq!(out[1], "List.map : (f : α → β) : List α → List β");
        // どの module にあるかも表示する。今の環境に無い名前も結果に含まれるため。
        assert_eq!(out[2], "  Init.Prelude");
        assert!(out[3].starts_with("List.mapTR : "), "{}", out[3]);
    }

    /// ja: 件数を絞ったときは、全体の件数と表示した件数を表示する
    /// en: Shows the total and the shown count when the results are limited
    #[story("D4")]
    #[test]
    fn shows_the_total_and_the_shown_count_when_the_results_are_limited() {
        // loogle は結果を 200 件で打ち切って返すので、count は hits より多いことがある。
        let got = json!({"count": 360, "hits": hits()});
        let head = pure::loogle_text(&got, 1, 100, Style::PLAIN);
        assert_eq!(head.split('\n').next(), Some("360 件 (先頭 1 件)"));
        let all = text(&json!({"count": 2, "hits": hits()}));
        assert_eq!(all.matches('\n').count(), 4);
    }

    /// ja: 端末の幅より長い行は、幅に合わせて … で切り詰める
    /// en: Truncates a line longer than the terminal width with an ellipsis
    #[story("D4")]
    #[test]
    fn truncates_a_line_longer_than_the_terminal_width_with_an_ellipsis() {
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

    /// ja: 結果が 0 件なら、見つからなかったと表示する
    /// en: Shows that nothing was found for zero results
    #[story("D4")]
    #[test]
    fn shows_that_nothing_was_found_for_zero_results() {
        assert_eq!(text(&json!({"count": 0, "hits": []})), "見つからなかった");
    }

    /// ja: loogle のエラーは翻訳せずに表示し、候補を添える
    /// en: Shows a loogle error untranslated with its suggestions
    #[story("D4")]
    #[test]
    fn shows_a_loogle_error_untranslated_with_its_suggestions() {
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

    /// ja: クエリの記号を URL エンコードして送る
    /// en: Sends the query with its symbols URL-encoded
    #[story("D4")]
    #[test]
    fn sends_the_query_with_its_symbols_url_encoded() {
        // `?a + ?b` の `+` をそのまま URL に含めると、loogle 側では空白として
        // 解釈される。
        let (url, got) = serve("200 OK", r#"{"count": 0, "hits": []}"#);
        ask(&url, "?a + ?b").expect("loogle");
        let line = got.recv().expect("request");
        assert!(line.starts_with("GET /json?q=%3Fa%20%2B%20%3Fb "), "{line}");
    }

    /// ja: 応答が無ければ SearchError を返す
    /// en: Returns a SearchError if the server does not respond
    #[story("D4")]
    #[test]
    fn returns_a_searcherror_if_the_server_does_not_respond() {
        // ネットワークの問題で REPL を終了させない。呼び出し側がエラーを受け取って
        // 1 行で報告する。
        let closed = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/json", closed.local_addr().expect("addr"));
        drop(closed);
        assert!(ask(&url, "Nat").is_err());
    }

    /// ja: HTTP のエラーは、コードと理由を SearchError として返す
    /// en: Returns an HTTP error as a SearchError with its code and reason
    #[story("D4", "F4")]
    #[test]
    fn returns_an_http_error_as_a_searcherror_with_its_code_and_reason() {
        let (url, _) = serve("503 Service Unavailable", "");
        let err = ask(&url, "Nat").expect_err("503");
        assert_eq!(err.to_string(), "503 Service Unavailable");
    }

    /// ja: 応答が JSON のオブジェクトでなければ SearchError を返す
    /// en: Returns a SearchError if the response is not a JSON object
    #[story("D4", "F4")]
    #[test]
    fn returns_a_searcherror_if_the_response_is_not_a_json_object() {
        let (url, _) = serve("200 OK", "[1, 2]");
        let err = ask(&url, "Nat").expect_err("配列");
        assert!(err.to_string().contains("オブジェクトではない"), "{err}");
    }
}

mod property_list {
    //! ja: 性質一覧
    //! en: Property list
    //!
    //! SPEC.ja.md と SPEC.en.md は、テストの doc の ja: と en: の文から生成する (crates/spec)。
    //! 手で書き足す場所を作らない代わりに、生成し忘れをこのテストで検出する。
    use super::*;

    /// ja: テストとストーリーの ja: の文から、SPEC.ja.md と同じ内容を生成する
    /// en: Generates the same content as SPEC.ja.md from the ja lines of the tests and stories
    #[story("X1")]
    #[test]
    fn generates_the_same_content_as_spec_ja_md_from_the_ja_lines_of_the_tests_and_stories() {
        if let Err(why) = spec::check(&spec::root(), spec::Lang::Ja) {
            panic!("{why}");
        }
    }

    /// ja: テストとストーリーの en: の文から、SPEC.en.md と同じ内容を生成する
    /// en: Generates the same content as SPEC.en.md from the en lines of the tests and stories
    #[story("X2")]
    #[test]
    fn generates_the_same_content_as_spec_en_md_from_the_en_lines_of_the_tests_and_stories() {
        if let Err(why) = spec::check(&spec::root(), spec::Lang::En) {
            panic!("{why}");
        }
    }
}

mod detecting_lake_projects {
    //! ja: Lake プロジェクトの判定
    //! en: Detecting Lake projects
    //!
    //! 設定に環境が無いとき、カレントディレクトリから Lake プロジェクトを探す。
    use super::*;

    fn libs(dir: &Path, file: &str, text: &str) -> Vec<String> {
        fs::write(dir.join(file), text).expect("write");
        config::lake_libs(&path_str(dir))
    }

    /// ja: サブディレクトリから探すと、lakefile のある親ディレクトリを返す
    /// en: Returns the parent directory with a lakefile when searching from a subdirectory
    #[story("G6")]
    #[test]
    fn returns_the_parent_directory_with_a_lakefile_when_searching_from_a_subdirectory() {
        let dir = tmp();
        fs::write(dir.path().join("lakefile.toml"), "").expect("write");
        let sub = dir.path().join("A").join("B");
        fs::create_dir_all(&sub).expect("mkdir");
        assert_eq!(
            config::lake_root(&path_str(&sub)),
            Some(path_str(dir.path()))
        );
    }

    /// ja: lakefile が無ければ None を返す
    /// en: Returns None without a lakefile
    #[story("G3", "G6")]
    #[test]
    fn returns_none_without_a_lakefile() {
        let dir = tmp();
        assert_eq!(config::lake_root(&path_str(dir.path())), None);
    }

    /// ja: lakefile.toml から lean_lib の名前を返す
    /// en: Returns the lean_lib names from lakefile.toml
    #[story("G6")]
    #[test]
    fn returns_the_lean_lib_names_from_lakefile_toml() {
        let dir = tmp();
        let text = "name = \"p\"\n[[lean_lib]]\nname = \"Foo\"\n[[lean_lib]]\nname = \"Bar\"\n";
        assert_eq!(libs(dir.path(), "lakefile.toml", text), ["Foo", "Bar"]);
    }

    /// ja: lakefile.lean から lean_lib の名前を返す
    /// en: Returns the lean_lib names from lakefile.lean
    #[story("G6")]
    #[test]
    fn returns_the_lean_lib_names_from_lakefile_lean() {
        let dir = tmp();
        let text = "import Lake\nopen Lake DSL\n\nlean_lib Foo where\nlean_lib «Bar.Baz»\n";
        assert_eq!(libs(dir.path(), "lakefile.lean", text), ["Foo", "Bar.Baz"]);
    }

    /// ja: TOML として読めない lakefile.toml からは、空を返す
    /// en: Returns nothing from a lakefile.toml that is not valid TOML
    #[story("G4", "G6")]
    #[test]
    fn returns_nothing_from_a_lakefile_toml_that_is_not_valid_toml() {
        let dir = tmp();
        assert!(libs(dir.path(), "lakefile.toml", "[[lean_lib]\n").is_empty());
    }

    /// ja: 設定が無ければ、上にある Lake プロジェクトのディレクトリ名と lean_lib で環境を作る
    /// en: Names the environment after the enclosing Lake project and imports its lean_lib without a config
    #[story("G6")]
    #[test]
    fn names_the_environment_after_the_enclosing_lake_project_and_imports_its_lean_lib_without_a_config()
     {
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

    /// ja: -p のパスの書き方によらず、プロジェクトのディレクトリ名を環境の名前にする
    /// en: Names the environment after the project directory for any form of the -p path
    #[story("G6")]
    #[test]
    fn names_the_environment_after_the_project_directory_for_any_form_of_the_p_path() {
        let dir = tmp();
        let proj = dir.path().join("myproj");
        fs::create_dir_all(dir.path().join("other")).expect("mkdir");
        fs::create_dir_all(&proj).expect("mkdir");
        fs::write(
            proj.join("lakefile.toml"),
            "[[lean_lib]]\nname = \"MyProj\"\n",
        )
        .expect("write");
        let home = path_str(dir.path());
        let cases = [
            ("../myproj/", path_str(&dir.path().join("other"))),
            (".", path_str(&proj)),
            ("~/myproj", "/".to_string()),
        ];
        for (arg, cwd) in cases {
            let ctx = ctx_with(&[("HOME", &home)], &cwd);
            let got = config::resolve(&ctx, None, Some(arg), &[], &table("")).expect("resolve");
            assert_eq!(
                (got.name.as_str(), got.project, got.imports),
                ("myproj", Some(path_str(&proj)), vec!["MyProj".to_string()]),
                "-p {arg}"
            );
        }
    }

    /// ja: Lake プロジェクトの外では、プロジェクトの無い plain という環境にする
    /// en: Uses the plain environment with no project outside a Lake project
    #[story("G3", "G6")]
    #[test]
    fn uses_the_plain_environment_with_no_project_outside_a_lake_project() {
        let dir = tmp();
        let ctx = ctx_at(&path_str(dir.path()));
        let got = config::resolve(&ctx, None, None, &[], &table("")).expect("resolve");
        assert_eq!((got.name.as_str(), got.project), ("plain", None));
    }

    /// ja: import を指定すれば、lean_lib の代わりにそれを使う
    /// en: Uses the given imports instead of lean_lib
    #[story("G6")]
    #[test]
    fn uses_the_given_imports_instead_of_lean_lib() {
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

mod command_line_options {
    //! ja: 起動オプション
    //! en: Command-line options
    use super::*;

    fn parse(argv: &[&str]) -> Args {
        parse_args(argv).expect("parse_args")
    }

    /// ja: -i と --import を繰り返すと、すべての import を順に返す
    /// en: Returns every import in order for repeated -i and --import
    #[story("G6")]
    #[test]
    fn returns_every_import_in_order_for_repeated_i_and_import() {
        assert_eq!(parse(&["-i", "A", "--import", "B"]).imports, ["A", "B"]);
    }

    /// ja: -e か --env の値を環境の名前として返す
    /// en: Takes the value of -e or --env as the environment name
    #[story("G5")]
    #[test]
    fn takes_the_value_of_e_or_env_as_the_environment_name() {
        assert_eq!(parse(&["-e", "m"]).name.as_deref(), Some("m"));
        assert_eq!(parse(&["--env", "m"]).name.as_deref(), Some("m"));
    }

    /// ja: 値の無い -e、-i、-p には、オプションの名前を含むエラーを返す
    /// en: Returns an error naming the option for -e, -i, or -p without a value
    #[story("G5", "G6")]
    #[test]
    fn returns_an_error_naming_the_option_for_e_i_or_p_without_a_value() {
        // 終了コードは bin が決める。ここではメッセージだけを確かめる。
        for flag in ["-e", "-i", "-p"] {
            let why = parse_args(&[flag]).expect_err("エラーにならない");
            assert!(why.contains(&format!("{flag} には")), "{why}");
        }
    }

    /// ja: 不明なオプションには、そのオプションを不明と書いたエラーを返す
    /// en: Returns an error naming an unknown option
    #[story("H1")]
    #[test]
    fn returns_an_error_naming_an_unknown_option() {
        let why = parse_args(&["--nosuch"]).expect_err("エラーにならない");
        assert!(why.contains("不明なオプション: --nosuch"), "{why}");
    }

    /// ja: ファイル名を、読み込むファイルとして返す
    /// en: Takes a file name as the file to load
    #[story("B4")]
    #[test]
    fn takes_a_file_name_as_the_file_to_load() {
        assert_eq!(parse(&["foo.lean"]).preload.as_deref(), Some("foo.lean"));
    }

    /// ja: ファイルを 2 つ指定すると、1 つだけと書いたエラーを返す
    /// en: Returns an error that only one file is allowed for two file names
    #[story("B4")]
    #[test]
    fn returns_an_error_that_only_one_file_is_allowed_for_two_file_names() {
        // 後のファイルだけを読み込むと、前のファイルを読み込んでいないことに気づけない。
        let why = parse_args(&["a.lean", "b.lean"]).expect_err("エラーにならない");
        assert!(why.contains("ファイルは 1 つだけ"), "{why}");
    }
}

mod startup {
    //! ja: 起動
    //! en: Startup
    //!
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

    /// ja: エンジンが用意済みなら、leani --setup を実行すると用意せずに用意済みと表示する
    /// en: Shows that the engine is ready without setting it up when leani --setup runs with a built engine
    #[story("G3")]
    #[test]
    fn shows_that_the_engine_is_ready_without_setting_it_up_when_leani_setup_runs_with_a_built_engine()
     {
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

    /// ja: エンジンが無ければ、LEANI_NO_SETUP があっても leani --setup でエンジンを用意する
    /// en: Sets up the engine when leani --setup runs without one, even with LEANI_NO_SETUP
    #[story("G3")]
    #[test]
    fn sets_up_the_engine_when_leani_setup_runs_without_one_even_with_leani_no_setup() {
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

    /// ja: leani を実行してエンジンが起動しなければ、起動しなかったと表示して exit 1 で終わる
    /// en: Shows that the engine did not start and exits with 1 if the engine fails to start
    #[story("G4")]
    #[test]
    fn shows_that_the_engine_did_not_start_and_exits_with_1_if_the_engine_fails_to_start() {
        let dir = tmp();
        let eng = built_engine(dir.path(), "leanprover/lean4:v1");
        config(dir.path(), &with_engine(&eng));
        // elan がすぐ終了するので、エンジンは最初の応答を返さずに終了する。
        let path = fake_tools(dir.path(), &[("elan", "exit 1"), ("lake", "")]);

        let got = run(&mut isolated(dir.path(), &[("PATH", &path)]), "");

        assert_eq!(got.code, 1, "{}", got.out);
        assert!(got.err.contains("エンジンが起動しなかった"), "{}", got.err);
    }

    /// ja: 設定ファイルを読めないか起動できない理由があれば、起動せずに理由を表示して exit 1 で終わる
    /// en: Shows why and exits with 1 without starting if the config is unreadable or startup is blocked
    #[story("G4")]
    #[test]
    fn shows_why_and_exits_with_1_without_starting_if_the_config_is_unreadable_or_startup_is_blocked()
     {
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

    /// ja: ファイルを指定して leani を実行すると、そのファイルの宣言を使った入力を評価する
    /// en: Evaluates input that uses declarations from the file when leani runs with a file
    #[story("B4")]
    #[test]
    fn evaluates_input_that_uses_declarations_from_the_file_when_leani_runs_with_a_file() {
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

    /// ja: 出力先が閉じると、panic せずに exit 0 で終わる
    /// en: Exits with 0 without panicking when the output is closed
    #[story("F4")]
    #[test]
    fn exits_with_0_without_panicking_when_the_output_is_closed() {
        // `leani < file | head -1` で head が先に終了したとき。書けないまま続けても
        // 誰も読まないので、入力の残りは実行せずに終わる。
        let dir = tmp();
        config(dir.path(), NO_ENGINE);
        let mut child = real(dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        drop(child.stdout.take());
        let mut stdin = child.stdin.take().expect("stdin");
        // leani が先に終了していたら書けないが、そのときも終了コードで確かめる。
        let _ = stdin.write_all(b"1 + 1\n2 + 2\n");
        drop(stdin);
        let got = child.wait_with_output().expect("wait");
        let err = String::from_utf8_lossy(&got.stderr);
        assert!(!err.contains("panicked"), "{err}");
        assert_eq!(got.status.code(), Some(0), "{err}");
    }

    /// 端末の Ctrl-C は leani にだけ届く (子プロセスは別のセッションにある)。
    /// そこで、偽のコマンドから leani に SIGINT を送る。
    const SEND_SIGINT: &str = "kill -INT $PPID; sleep 30";

    /// ja: エンジンの用意の途中で Ctrl-C を押すと、手動の手順を表示せずに exit 130 で終わる
    /// en: Exits with 130 without the manual steps when Ctrl-C is pressed during engine setup
    #[story("G3")]
    #[test]
    fn exits_with_130_without_the_manual_steps_when_ctrl_c_is_pressed_during_engine_setup() {
        let dir = tmp();
        config(dir.path(), NO_ENGINE);
        let lean = "echo 'Lean (version 4.1.0, arm64-apple-darwin, commit 0, Release)'";
        let path = fake_tools(
            dir.path(),
            &[
                ("elan", ""),
                ("git", SEND_SIGINT),
                ("lake", ""),
                ("lean", lean),
            ],
        );

        let got = run(isolated(dir.path(), &[("PATH", &path)]).arg("--setup"), "");

        assert_eq!(got.code, 130, "{}{}", got.out, got.err);
        assert!(!got.err.contains("手動で用意する場合"), "{}", got.err);
    }

    /// ja: バージョンを調べている途中で Ctrl-C を押すと、exit 130 で終わる
    /// en: Exits with 130 when Ctrl-C is pressed while checking the version
    #[story("G3")]
    #[test]
    fn exits_with_130_when_ctrl_c_is_pressed_while_checking_the_version() {
        // elan は lean --version の中で toolchain を取得することがあるので、ここでも
        // 時間がかかる。中断を「バージョンが分からない」にすると exit 1 で終わる。
        let dir = tmp();
        config(dir.path(), NO_ENGINE);
        let path = fake_tools(
            dir.path(),
            &[
                ("elan", ""),
                ("git", ""),
                ("lake", ""),
                ("lean", SEND_SIGINT),
            ],
        );

        let got = run(isolated(dir.path(), &[("PATH", &path)]).arg("--setup"), "");

        assert_eq!(got.code, 130, "{}{}", got.out, got.err);
    }

    /// ja: -V を付けて実行すると、環境と Lean のバージョンと設定ファイルの場所を表示する
    /// en: Shows the environment, the Lean version, and the config file path when leani -V runs
    #[story("G7")]
    #[test]
    fn shows_the_environment_the_lean_version_and_the_config_file_path_when_leani_v_runs() {
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

mod abbreviation_list {
    //! ja: 略記の一覧
    //! en: Abbreviation list
    //!
    //! `:abbrev` の表示。`man ascii` のように、キーと記号の組を複数の列に並べる。
    use super::*;

    /// ja: 入力で始まるキーの略記だけを、先頭のバックスラッシュの有無によらず返す
    /// en: Returns abbreviations whose keys start with the input, with or without a leading backslash
    #[story("C7")]
    #[test]
    fn returns_abbreviations_whose_keys_start_with_the_input_with_or_without_a_leading_backslash() {
        let got = abbrev_lookup("le");
        assert!(got.contains(&("le", "≤")), "{got:?}");
        assert!(got.iter().all(|(k, _)| k.starts_with("le")), "{got:?}");
        assert_eq!(abbrev_lookup("\\le"), got);
    }

    /// ja: 記号には、その記号に変換される略記を返す
    /// en: Returns the abbreviations that convert to a given symbol
    #[story("C7")]
    #[test]
    fn returns_the_abbreviations_that_convert_to_a_given_symbol() {
        let keys: Vec<&str> = abbrev_lookup("→").into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["->", "imp", "r", "r-", "rightarrow", "to"]);
    }

    /// ja: 列の下まで並べたら、右の列に続ける
    /// en: Continues in the next column after filling a column to the bottom
    #[story("C7")]
    #[test]
    fn continues_in_the_next_column_after_filling_a_column_to_the_bottom() {
        let pairs = [("a", "α"), ("b", "β"), ("c", "γ")];
        let got = abbrev_table(&pairs, 13);
        assert_eq!(
            got.split('\n').collect::<Vec<_>>(),
            ["\\a α   \\c γ", "\\b β"]
        );
    }

    /// ja: 全角の文字を 2 文字分の幅として列をそろえる
    /// en: Aligns columns counting a full-width character as two cells wide
    #[story("C7")]
    #[test]
    fn aligns_columns_counting_a_full_width_character_as_two_cells_wide() {
        let pairs = [("x", "あ"), ("y", "b"), ("z", "c"), ("w", "d")];
        let got = abbrev_table(&pairs, 12);
        assert_eq!(
            got.split('\n').collect::<Vec<_>>(),
            ["\\x あ   \\z c", "\\y b    \\w d"]
        );
    }

    /// ja: 幅が広いほど、幅に収まる範囲で列を増やす
    /// en: Adds as many columns as the width allows
    #[story("C7")]
    #[test]
    fn adds_as_many_columns_as_the_width_allows() {
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
