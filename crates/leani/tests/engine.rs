//! `Engine` と `Repl` を直接呼ぶテスト。1 テストに数秒かかる。

mod common;

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

use common::{
    Driver, FakeSearch, PANIC, Sink, cmd, die_once, fresh, interrupt_after_write, is_tactic,
    sandbox, with_info, with_panic,
};
use leani::cli::{Line, LineSource};
use leani::engine::{Engine, Hooked, Stage};
use leani::repl::Repl;
use leani_core::pure::balanced;
use leani_core::types::{EngineFault, Json};
use leani_read::config::{EnvConfig, load_config, resolve};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::json;
use story::story;

/// 表示を受け取るだけの出口。エンジンを用意するときの表示はテストでは確かめない。
fn sink() -> Vec<u8> {
    Vec::new()
}

fn died_message(r: Result<impl std::fmt::Debug, EngineFault>) -> String {
    match r {
        Err(EngineFault::Died(why)) => why,
        other => panic!("Died にならない: {other:?}"),
    }
}

mod startup_checks {
    //! ja: 起動時の確認
    //! en: Startup checks
    //!
    //! boot と、エンジンの異常終了の報告。
    use super::*;

    /// ja: 解決できない import を指定して起動すると、import に失敗したと報告して起動を中止する
    /// en: Aborts startup and reports the import failure when started with an unresolvable import
    #[story("G4")]
    #[test]
    fn aborts_startup_and_reports_the_import_failure_when_started_with_an_unresolvable_import() {
        let sb = sandbox();
        let cfg = EnvConfig::make(
            &sb.ctx,
            "bogus",
            None,
            &["NoSuchModuleXYZ".to_string()],
            None,
            None,
        );
        let mut eng = Engine::new(&sb.ctx, cfg, &mut sink()).expect("起動できない");
        let why = died_message(eng.boot());
        assert!(why.contains("import に失敗した"), "{why}");
    }

    /// ja: エンジンがシグナルで終了したあとにコマンドを送ると、報告にそのシグナルの名前を含める
    /// en: Names the signal in the report when a command is sent after the engine is killed by a signal
    #[story("G4")]
    #[test]
    fn names_the_signal_in_the_report_when_a_command_is_sent_after_the_engine_is_killed_by_a_signal()
     {
        let sb = sandbox();
        let cfg = EnvConfig::make(&sb.ctx, "bogus", None, &[], None, None);
        let mut eng = Engine::new(&sb.ctx, cfg, &mut sink()).expect("起動できない");
        let pid = eng.pid().expect("プロセスが無い");
        kill(Pid::from_raw(pid.cast_signed()), Signal::SIGKILL).expect("kill");
        let why = died_message(eng.send_cmd("def after_kill := 1", true));
        assert!(why.contains("SIGKILL"), "{why}");
    }

    /// ja: :env で切り替えた先のエンジンが起動に失敗すると、そのプロセスを終了させ、元の環境で起動し直したエンジンは動いたままにする
    /// en: Stops the failed engine and keeps the engine restarted in the original environment running when the engine for :env fails to start
    #[story("G4", "G5")]
    #[test]
    fn stops_the_failed_engine_and_keeps_the_engine_restarted_in_the_original_environment_running_when_the_engine_for_env_fails_to_start()
     {
        // Engine を作った時点で repl は起動している。boot が失敗したあとに
        // Repl がエンジンを差し替えると、終了させる方法が無いままプロセスが残る。
        let mut repl = Driver::new();
        only_core(&mut repl);
        repl.feed(&[":env wide"]);

        let failed = repl.repl.failed_engines_mut();
        assert_eq!(failed.len(), 1, "boot に失敗したエンジンは 1 つ");
        assert!(!failed[0].alive(), "起動に失敗したエンジンが残った");
        assert!(
            repl.repl.eng_mut().alive(),
            "戻った先のエンジンまで終了させた"
        );
    }
}

mod evaluating_expressions {
    //! ja: 式の評価
    //! en: Evaluating expressions
    use super::*;

    /// ja: 式を入力すると、評価した値を表示する
    /// en: Shows the value when an expression is entered
    #[story("A1")]
    #[test]
    fn shows_the_value_when_an_expression_is_entered() {
        let mut repl = Driver::new();
        assert!(repl.feed(&["1 + 1"]).contains('2'));
    }

    /// ja: 改ページを含む文字列の式を入力すると、その値を表示する
    /// en: Shows the value when an expression with a form feed in a string is entered
    #[story("A1", "C1")]
    #[test]
    fn shows_the_value_when_an_expression_with_a_form_feed_in_a_string_is_entered() {
        // 完結の判定に送る文字列リテラルに \f と書くと、Lean が読めずに判定が
        // エラーになり、式を宣言として送ってしまう。
        let mut repl = Driver::new();
        let out = repl.feed(&["\"a\u{c}b\".length"]);
        assert!(out.lines().any(|l| l.trim() == "3"), "{out}");
    }

    /// ja: 2 行にまたがる文字列の式を入力すると、文字列を変えずに評価した値を表示する
    /// en: Shows the value of the unchanged string when an expression with a multi-line string is entered
    #[story("A1")]
    #[test]
    fn shows_the_value_of_the_unchanged_string_when_an_expression_with_a_multi_line_string_is_entered()
     {
        // #eval に包むときに 2 行目の先頭に空白を足すと、文字列の値が変わる。
        let mut repl = Driver::new();
        let out = repl.feed(&["\"Hello", "World\".length"]);
        assert!(out.lines().any(|l| l.trim() == "11"), "{out}");
    }

    /// ja: IO の式を入力すると、その action を実行して出力を表示する
    /// en: Runs the action and shows its output when an IO expression is entered
    #[story("A1", "B1")]
    #[test]
    fn runs_the_action_and_shows_its_output_when_an_io_expression_is_entered() {
        let mut repl = Driver::new();
        assert!(repl.feed(&["IO.println \"hi\""]).contains("hi"));
    }

    /// ja: Repr の無い項を入力すると、評価できない理由と型を表示する
    /// en: Shows why it cannot be evaluated and the type when a term without Repr is entered
    #[story("A2")]
    #[test]
    fn shows_why_it_cannot_be_evaluated_and_the_type_when_a_term_without_repr_is_entered() {
        // Nat -> Nat に Repr が無いので #eval できない。
        let mut repl = Driver::new();
        let out = repl.feed(&["Nat.succ"]);
        assert!(out.contains("Repr"), "{out}");
        assert!(out.contains("Nat.succ (n : Nat) : Nat"), "{out}");
    }

    /// ja: noncomputable な定数を入力すると、評価できない理由と型を表示する
    /// en: Shows why it cannot be evaluated and the type when a noncomputable constant is entered
    #[story("A2")]
    #[test]
    fn shows_why_it_cannot_be_evaluated_and_the_type_when_a_noncomputable_constant_is_entered() {
        let mut repl = Driver::new();
        repl.feed(&["noncomputable def nc : Nat := Classical.choice ⟨1⟩"]);
        let out = repl.feed(&["nc"]);
        assert!(out.contains("noncomputable なので評価できない"), "{out}");
        assert!(out.contains("nc : Nat"), "{out}");
    }

    /// ja: BaseIO と IO の action を混ぜた do ブロックを入力すると、エラーにせず IO として実行する
    /// en: Runs the block as IO without an error when a do block mixing BaseIO and IO actions is entered
    #[story("A1", "B1")]
    #[test]
    fn runs_the_block_as_io_without_an_error_when_a_do_block_mixing_baseio_and_io_actions_is_entered()
     {
        // IO.getEnv は BaseIO なので、Lean は do 全体を BaseIO と決めてしまい、
        // IO.println を書けなくなる。
        let mut repl = Driver::new();
        let out = repl.feed(&[
            "do let x ← IO.getEnv \"LEANI_NO_SUCH_VAR\"; IO.println s!\"none={x.isNone}\"",
        ]);
        assert!(out.contains("none=true"), "{out}");
        assert!(!out.to_lowercase().contains("error"), "{out}");
    }

    /// ja: do の外で ← を使った式を入力すると、do で包んでエラーにせず実行する
    /// en: Wraps it in do and runs it without an error when an expression with a left arrow is entered
    #[story("A1", "B1")]
    #[test]
    fn wraps_it_in_do_and_runs_it_without_an_error_when_an_expression_with_a_left_arrow_is_entered()
    {
        let mut repl = Driver::new();
        let path = repl.tmp().join("note.txt");
        std::fs::write(&path, "from file\n").expect("write");
        let out = repl.feed(&[&format!(
            "IO.print (← IO.FS.readFile \"{}\")",
            path.display()
        )]);
        assert!(out.contains("from file"), "{out}");
        assert!(!out.to_lowercase().contains("error"), "{out}");
    }

    /// ja: BaseIO の action に ← を使った式を入力すると、エラーにせず IO として実行する
    /// en: Runs it as IO without an error when an expression with a left arrow on a BaseIO action is entered
    #[story("A1", "B1")]
    #[test]
    fn runs_it_as_io_without_an_error_when_an_expression_with_a_left_arrow_on_a_baseio_action_is_entered()
     {
        let mut repl = Driver::new();
        let out =
            repl.feed(&["IO.println s!\"none={(← IO.getEnv \"LEANI_NO_SUCH_VAR\").isNone}\""]);
        assert!(out.contains("none=true"), "{out}");
        assert!(!out.to_lowercase().contains("error"), "{out}");
    }

    /// ja: ← を含む式がエラーになると、元の入力の中のエラーの位置を表示する
    /// en: Shows the error position within the original input when an expression with a left arrow fails
    #[story("A5")]
    #[test]
    fn shows_the_error_position_within_the_original_input_when_an_expression_with_a_left_arrow_fails()
     {
        let mut repl = Driver::new();
        let out = repl.feed(&["IO.println (← pure (1 + true))"]);
        // 1 + true の位置。do で包んだ分 (1 行 4 桁) を戻していなければずれる。
        assert!(
            out.contains("1:21  IO.println (← pure (1 + true))"),
            "{out}"
        );
        let caret = format!("\n{}{}\n", " ".repeat(26), "^".repeat(8));
        assert!(out.contains(&caret), "{out}");
    }

    /// ja: ファイルを読む do ブロックを入力すると、ファイルの中身を表示する
    /// en: Shows the file contents when a do block that reads a file is entered
    #[story("B1")]
    #[test]
    fn shows_the_file_contents_when_a_do_block_that_reads_a_file_is_entered() {
        let mut repl = Driver::new();
        let path = repl.tmp().join("note.txt");
        std::fs::write(&path, "from file\n").expect("write");
        let out = repl.feed(&[&format!(
            "do let s ← IO.FS.readFile \"{}\"; IO.print s",
            path.display()
        )]);
        assert!(out.contains("from file"), "{out}");
    }

    /// ja: プロセスを起動する do ブロックを入力すると、エラーにせずプロセスの出力を表示する
    /// en: Shows the process output without an error when a do block that starts a process is entered
    #[story("B1")]
    #[test]
    fn shows_the_process_output_without_an_error_when_a_do_block_that_starts_a_process_is_entered()
    {
        let mut repl = Driver::new();
        let out = repl.feed(&[
            "do let r ← IO.Process.output {cmd := \"echo\", args := #[\"hi\"]}; IO.print r.stdout",
        ]);
        assert!(out.contains("hi"), "{out}");
        assert!(!out.to_lowercase().contains("error"), "{out}");
    }
}

mod multi_line_declarations {
    //! ja: 複数行の宣言
    //! en: Multi-line declarations
    use super::*;

    /// ja: 複数行の宣言のあとに空行を入力すると、まとめて 1 つの宣言として実行する
    /// en: Runs the lines as one declaration when a multi-line declaration is followed by a blank line
    #[story("C1")]
    #[test]
    fn runs_the_lines_as_one_declaration_when_a_multi_line_declaration_is_followed_by_a_blank_line()
    {
        let mut repl = Driver::new();
        repl.block(&[
            "def fib : Nat -> Nat",
            "  | 0 => 0",
            "  | 1 => 1",
            "  | n+2 => fib n + fib (n+1)",
        ]);
        assert!(repl.feed(&["fib 10"]).contains("55"));
        assert_eq!(repl.declarations().len(), 1);
    }

    /// ja: 1 行の宣言の直後にインデントした行を入力すると、その宣言の続きとして扱う
    /// en: Continues a one-line declaration when an indented line is entered right after it
    #[story("C2")]
    #[test]
    fn continues_a_one_line_declaration_when_an_indented_line_is_entered_right_after_it() {
        let mut repl = Driver::new();
        repl.feed(&["def three := 1"]);
        repl.block(&["  + 2"]);
        assert!(repl.feed(&["three"]).contains('3'));
        assert_eq!(
            repl.declarations().len(),
            1,
            "前の入力の続きとして扱わずに 2 件になった"
        );
    }

    /// ja: 空行で確定したブロックの直後にインデントした行を入力すると、そのブロックの続きとして扱う
    /// en: Continues a block confirmed by a blank line when an indented line is entered right after it
    #[story("C2")]
    #[test]
    fn continues_a_block_confirmed_by_a_blank_line_when_an_indented_line_is_entered_right_after_it()
    {
        let mut repl = Driver::new();
        repl.block(&["def pick : Nat → Nat", "  | 0 => 10"]);
        repl.block(&["  | _ => 20"]);
        assert!(repl.feed(&["pick 3"]).contains("20"));
        assert_eq!(
            repl.declarations().len(),
            1,
            "前の入力の続きとして扱わずに 2 件になった"
        );
    }

    /// ja: エラーになる宣言を入力すると、その宣言を環境に加えない
    /// en: Leaves the declaration out of the environment when a declaration with an error is entered
    #[story("C1")]
    #[test]
    fn leaves_the_declaration_out_of_the_environment_when_a_declaration_with_an_error_is_entered() {
        let mut repl = Driver::new();
        repl.feed(&["def broken : Nat := \"oops\""]);
        assert!(repl.declarations().is_empty());
    }

    /// ja: 空行を含む入力を :{ と :} で囲むと、:} を入力するまで実行せず 1 つの宣言として実行する
    /// en: Runs one declaration only after :} when input with a blank line is between :{ and :}
    #[story("C6")]
    #[test]
    fn runs_one_declaration_only_after_when_input_with_a_blank_line_is_between_and() {
        let mut repl = Driver::new();
        repl.feed(&[":{", "def gap : Nat :=", "", "  5"]);
        assert!(repl.declarations().is_empty(), ":}} の前に実行した");
        repl.feed(&[":}"]);
        assert!(repl.feed(&["gap"]).contains('5'));
        assert_eq!(repl.declarations().len(), 1);
    }

    /// ja: :{ のすぐあとに :} を入力すると、何も実行せず何も表示しない
    /// en: Runs and shows nothing when :} is entered right after :{
    #[story("C6")]
    #[test]
    fn runs_and_shows_nothing_when_is_entered_right_after() {
        let mut repl = Driver::new();
        assert_eq!(repl.feed(&[":{", ":}"]), "");
        assert!(repl.declarations().is_empty());
    }
}

mod end_of_a_notebook_cell {
    //! ja: ノートブックのセルの終わり
    //! en: End of a notebook cell
    use super::*;

    /// ja: 入力途中の宣言があるときにセルが終わると、その宣言を実行する
    /// en: Runs the unfinished declaration when the cell ends
    #[story("J1")]
    #[test]
    fn runs_the_unfinished_declaration_when_the_cell_ends() {
        let mut repl = Driver::new();
        repl.feed(&["def cellEnd : Nat :=", "  7"]);
        assert!(repl.declarations().is_empty(), "セルの終わりの前に実行した");
        repl.feed_with("end_block", Repl::end_block);
        assert_eq!(repl.declarations().len(), 1);
        assert!(repl.feed(&["cellEnd"]).contains('7'));
    }

    /// ja: :} で閉じていないブロックがあるときにセルが終わると、実行せずに :} が無いと表示する
    /// en: Shows that :} is missing without running the block when the cell ends inside :{
    #[story("J1", "C6")]
    #[test]
    fn shows_that_is_missing_without_running_the_block_when_the_cell_ends_inside() {
        let mut repl = Driver::new();
        repl.feed(&[":{", "def unclosed := 1"]);
        let out = repl.feed_with("end_block", Repl::end_block);
        assert!(out.contains(":} が無い"), "{out}");
        assert!(repl.declarations().is_empty());
        assert!(!repl.repl.explicit());
        assert!(repl.repl.buf().is_empty());
    }
}

/// 決めた行を順に返し、尽きたら入力の終わりを返す入力。
struct Script(std::vec::IntoIter<&'static str>);

impl LineSource for Script {
    fn read_line(&mut self, _prompt: &str) -> Line {
        match self.0.next() {
            Some(line) => Line::Text(line.to_string()),
            None => Line::End,
        }
    }
}

mod end_of_input {
    //! ja: 入力の終わり
    //! en: End of input
    //!
    //! パイプの EOF と端末の Ctrl-D。どちらも `Line::End` になる。
    use super::*;

    /// `lines` を `cli::run` に渡す。戻り値は、`run` の結果、表示、宣言の数。
    fn run(lines: Vec<&'static str>) -> (Result<i32, String>, String, usize) {
        let sb = sandbox();
        let table = load_config(&sb.ctx.config).expect("config");
        let cfg = resolve(&sb.ctx, None, None, &[], &table).expect("resolve");
        let out = Sink::default();
        let search = FakeSearch::default();
        let repl = Repl::new(&sb.ctx, cfg, None, Box::new(out.clone()), Box::new(search))
            .expect("起動できない");
        let repl = Mutex::new(repl);
        let got = leani::cli::run(&repl, &mut Script(lines.into_iter()));
        let mut repl = repl.into_inner().expect("repl");
        let decls = repl.eng().log().len();
        repl.close();
        let shown = out.0.lock().expect("sink").clone();
        (got, shown, decls)
    }

    /// ja: 入力途中のブロックがあるときに入力が終わると、そのブロックを実行する
    /// en: Runs the unfinished block when input ends
    #[story("C6", "F4")]
    #[test]
    fn runs_the_unfinished_block_when_input_ends() {
        // ファイルの末尾に空行が無いと、最後のブロックは入力途中のまま EOF になる。
        // 捨てると、ファイルの最後の宣言だけが表示なしに実行されない。
        let (got, out, _) = run(vec!["#eval", "  41 + 1"]);
        assert_eq!(got, Ok(0));
        assert!(out.lines().any(|l| l.trim() == "42"), "{out}");
    }

    /// ja: :} で閉じていないブロックがあるときに入力が終わると、実行せずに :} が無いと表示する
    /// en: Shows that :} is missing without running the block when input ends inside :{
    #[story("C6")]
    #[test]
    fn shows_that_is_missing_without_running_the_block_when_input_ends_inside() {
        // `:}` を書き忘れたのか、ここで終わるつもりだったのかは区別できない。
        let (got, out, decls) = run(vec![":{", "def unclosed := 1"]);
        assert_eq!(got, Ok(0));
        assert!(out.contains(":} が無い"), "{out}");
        assert_eq!(decls, 0);
    }
}

mod recovering_from_engine_crashes {
    //! ja: エンジンの異常終了からの復旧
    //! en: Recovering from engine crashes
    use super::*;

    /// ja: 宣言の実行中にエンジンが異常終了すると、再起動して、送っていた宣言を含めて再実行する
    /// en: Restarts and replays the declarations, including the one being sent, when the engine crashes
    #[story("F1")]
    #[test]
    fn restarts_and_replays_the_declarations_including_the_one_being_sent_when_the_engine_crashes()
    {
        let mut repl = Driver::new();
        repl.feed(&["def survivor := 9"]);
        let mut crashed = false;
        repl.hook(move |stage| match stage {
            Stage::Before(obj) if !crashed && cmd(obj).starts_with("def afterCrash") => {
                crashed = true;
                Hooked::Kill
            }
            _ => Hooked::Pass,
        });
        let out = repl.feed(&["def afterCrash := 3"]);
        assert!(out.contains("再起動する"), "{out}");
        assert!(out.contains("1 件を再実行"), "{out}");
        assert!(
            repl.feed(&["survivor"]).contains('9'),
            "replay されていない"
        );
        assert!(
            repl.feed(&["afterCrash"]).contains('3'),
            "異常終了のときに送っていた宣言が再実行されていない"
        );
        assert_eq!(
            repl.declarations(),
            ["def survivor := 9", "def afterCrash := 3"]
        );
    }

    /// ja: 中断のあとの再実行をもう一度中断すると、そのあとの入力には正しい値を表示する
    /// en: Shows correct values for later input when the replay after an interrupt is interrupted again
    #[story("A4", "F1")]
    #[test]
    fn shows_correct_values_for_later_input_when_the_replay_after_an_interrupt_is_interrupted_again()
     {
        let mut repl = Driver::new();
        repl.feed(&["def before := 5"]);
        let second = Arc::new(AtomicBool::new(false));
        let hit = second.clone();
        let mut first = false;
        repl.hook(move |stage| {
            let Stage::Written(obj) = stage else {
                return Hooked::Pass;
            };
            if !first && cmd(obj) == "#eval 0" {
                first = true;
                return Hooked::Interrupt;
            }
            // replay で宣言を送った直後にも中断する。
            if first && !hit.load(SeqCst) && cmd(obj) == "def before := 5" {
                hit.store(true, SeqCst);
                return Hooked::Interrupt;
            }
            Hooked::Pass
        });
        repl.feed(&["#eval 0"]);
        assert!(second.load(SeqCst), "replay の途中で中断していない");
        assert!(repl.feed(&["before"]).contains('5'));
        assert!(repl.feed(&["1 + 1"]).contains('2'));
    }

    /// ja: 宣言の実行でエンジンが PANIC すると、PANIC の内容を警告に表示し、宣言を環境に加えない
    /// en: Shows the PANIC in a warning and leaves the declaration out of the environment when the engine panics on it
    #[story("F1")]
    #[test]
    fn shows_the_panic_in_a_warning_and_leaves_the_declaration_out_of_the_environment_when_the_engine_panics_on_it()
     {
        // PANIC したあとのエンジンは状態がおかしくなっている可能性があるので、結果を
        // 普通の結果として扱わない。
        let mut repl = Driver::new();
        repl.hook(|stage| match stage {
            Stage::Before(obj) if cmd(obj).starts_with("def panicked") => Hooked::Reply(json!({
                "env": 99,
                "messages": [{ "severity": "info", "data": PANIC }],
            })),
            _ => Hooked::Pass,
        });
        let out = repl.feed(&["def panicked := 1"]);
        assert!(out.contains("PANIC した"), "{out}");
        assert!(out.contains("PANIC at Foo.bar"), "{out}");
        assert!(repl.declarations().is_empty());
    }

    /// ja: 式の評価でエンジンが PANIC すると、結果の代わりに警告を表示する
    /// en: Shows a warning instead of the result when the engine panics while evaluating an expression
    #[story("F1")]
    #[test]
    fn shows_a_warning_instead_of_the_result_when_the_engine_panics_while_evaluating_an_expression()
    {
        let mut repl = Driver::new();
        repl.hook(|stage| match stage {
            Stage::Before(obj)
                if cmd(obj).starts_with("#eval\n") && cmd(obj).contains("panicky") =>
            {
                Hooked::Reply(json!({
                    "messages": [{ "severity": "info", "data": format!("42\n{PANIC}") }],
                }))
            }
            _ => Hooked::Pass,
        });
        let out = repl.feed(&["panicky + 1"]);
        assert!(out.contains("PANIC した"), "{out}");
        assert!(!out.contains("42"), "{out}");
    }

    /// ja: 式、do ブロック、宣言のどれでエンジンが PANIC しても警告する
    /// en: Shows a warning when the engine panics on an expression, a do block or a declaration
    #[story("F1")]
    #[test]
    fn shows_a_warning_when_the_engine_panics_on_an_expression_a_do_block_or_a_declaration() {
        // 呼び出し側で検査を書き忘れた経路を見つけるため、エンジンとのやりとりの
        // 1 か所で応答に PANIC を足す。
        type Hit = fn(&str) -> bool;
        let cases: [(&str, Hit); 3] = [
            ("panicky + 1", |c| c.starts_with("#eval\n")),
            (
                "do let x ← IO.getEnv \"LEANI_NO_SUCH_VAR\"; IO.println s!\"{x}\"",
                |c| c.starts_with("#eval show IO"),
            ),
            ("def panicked := 1", |c| c.starts_with("def panicked")),
        ];
        let mut repl = Driver::new();
        for (line, hit) in cases {
            repl.hook(move |stage| match stage {
                Stage::Read(obj, resp) if hit(cmd(obj)) => Hooked::Reply(with_panic(resp)),
                _ => Hooked::Pass,
            });
            let out = repl.feed(&[line]);
            assert!(out.contains("PANIC した"), "{line}: {out}");
            assert!(repl.declarations().is_empty(), "{line}");
        }
    }

    /// ja: 毎回エンジンを異常終了させる宣言を送ると、再実行でも異常終了したと表示し、前の宣言を残す
    /// en: Reports the repeated crash and keeps earlier declarations when a declaration always crashes the engine
    #[story("F1")]
    #[test]
    fn reports_the_repeated_crash_and_keeps_earlier_declarations_when_a_declaration_always_crashes_the_engine()
     {
        // 同じ入力でエンジンが毎回異常終了する場合。再実行でも終了したあとに
        // 再起動しないと、終了したプロセスが残り、次の入力もすべて失敗する。
        let mut repl = Driver::new();
        repl.feed(&["def before := 5"]);
        repl.hook(|stage| match stage {
            Stage::Before(obj) if cmd(obj).starts_with("def poison") => Hooked::Kill,
            _ => Hooked::Pass,
        });
        let out = repl.feed(&["def poison := 1"]);
        assert!(out.contains("再実行してもエンジンが異常終了した"), "{out}");
        assert_eq!(repl.declarations(), ["def before := 5"]);
        assert!(repl.feed(&["before"]).contains('5'));
    }

    /// ja: 再起動を中断したあとに :restart を実行すると、宣言を再実行する
    /// en: Replays the declarations when :restart runs after a restart was interrupted
    #[story("A4", "F1")]
    #[test]
    fn replays_the_declarations_when_restart_runs_after_a_restart_was_interrupted() {
        let mut repl = Driver::new();
        repl.feed(&["def before := 5"]);
        repl.hook(|stage| match stage {
            Stage::Before(obj) if fresh(obj) => Hooked::Interrupt,
            _ => Hooked::Pass,
        });
        let out = repl.feed(&[":restart"]);
        assert!(out.contains("再起動を中断した"), "{out}");

        repl.unhook();
        assert!(repl.feed(&[":restart"]).contains("1 件を再実行"));
        assert!(repl.feed(&["before"]).contains('5'));
    }

    /// ja: ファイルの読み込み直しで再起動を中断したあとに :restart を実行すると、ファイルと宣言を戻す
    /// en: Restores the file and the declarations when :restart runs after reloading the file was interrupted
    #[story("A4", "F1")]
    #[test]
    fn restores_the_file_and_the_declarations_when_restart_runs_after_reloading_the_file_was_interrupted()
     {
        let mut repl = Driver::new();
        let path = file(&repl, "lib.lean", "def libA := 10\n");
        repl.feed(&[&format!(":l {path}")]);
        repl.feed(&["def usesA := libA + 1"]);

        interrupt_after_write(&mut repl, |o| cmd(o).contains("def libA"));
        let out = repl.feed(&[":restart"]);
        assert!(out.contains("再起動を中断した"), "{out}");
        // 読み込み直せなかった理由として報告すると、宣言の再実行まで進んで、
        // 止めたエンジンに送ることになる。
        assert!(!out.contains("再実行"), "{out}");
        assert_eq!(repl.repl.eng().unplayed(), ["def usesA := libA + 1"]);

        repl.unhook();
        let out = repl.feed(&[":restart"]);
        assert!(out.contains("1 件を再実行"), "{out}");
        assert!(repl.feed(&["usesA"]).contains("11"));
    }
}

mod when_the_engine_fails_to_restart {
    //! ja: エンジンを再起動できないとき
    //! en: When the engine fails to restart
    //!
    //! boot が失敗することもある。leani は報告して続ける (異常終了しない)。
    use super::*;

    /// ja: :restart のときにエンジンが起動に失敗すると、再起動できなかったと表示する
    /// en: Reports that the restart failed when the engine fails to start on :restart
    #[story("F1")]
    #[test]
    fn reports_that_the_restart_failed_when_the_engine_fails_to_start_on_restart() {
        let mut repl = Driver::new();
        repl.feed(&["def kept := 4"]);
        repl.hook(|stage| match stage {
            Stage::Before(obj) if fresh(obj) => Hooked::Kill,
            _ => Hooked::Pass,
        });
        let out = repl.feed(&[":restart"]);
        assert!(out.contains("再起動できなかった"), "{out}");
    }

    /// ja: 再起動に失敗すると、宣言を保留にし、次の :restart で再実行する
    /// en: Keeps the declarations pending and replays them on the next :restart when the restart fails
    #[story("F1")]
    #[test]
    fn keeps_the_declarations_pending_and_replays_them_on_the_next_restart_when_the_restart_fails()
    {
        // 捨てると、原因を直して再実行しても replay できなくなる。ただし log には
        // 残せない (env が無いので「環境に含まれている宣言」ではない)。
        let mut repl = Driver::new();
        repl.feed(&["def kept := 4"]);
        repl.hook(|stage| match stage {
            Stage::Before(obj) if fresh(obj) => Hooked::Kill,
            _ => Hooked::Pass,
        });
        repl.feed(&[":restart"]);
        assert_eq!(repl.repl.eng().unplayed(), ["def kept := 4"]);
        assert!(repl.repl.eng().log().is_empty());

        repl.unhook();
        assert!(repl.feed(&[":restart"]).contains("1 件を再実行"));
        assert!(repl.feed(&["kept"]).contains('4'));
    }
}

mod when_a_replay_stops_partway {
    //! ja: replay が途中で止まったとき
    //! en: When a replay stops partway
    //!
    //! 実行できなかった宣言は保留しておくが、環境に含まれている宣言の並び (log) には
    //! 混ぜない。log は env のスタックと 1 対 1 に対応していて、混ぜると :undo と
    //! sorry の置き換えが別の宣言を消してしまう。
    use super::*;

    /// ja: :restart の再実行を中断すると、残りの宣言を保留にし、環境の宣言に含めない
    /// en: Keeps the remaining declarations pending and out of the environment when the replay is interrupted
    #[story("F1", "F2")]
    #[test]
    fn keeps_the_remaining_declarations_pending_and_out_of_the_environment_when_the_replay_is_interrupted()
     {
        let mut repl = Driver::new();
        for one in ["def r1 := 1", "def r2 := 2", "def r3 := 3"] {
            repl.feed(&[one]);
        }

        interrupt_after_write(&mut repl, |o| cmd(o).starts_with("def r2"));
        let out = repl.feed(&[":restart"]); // Driver が毎行 INVARIANTS を見る

        assert!(out.contains("再実行しなかった宣言: 2 件"), "{out}");
        assert_eq!(repl.declarations(), ["def r1 := 1"]);
        // env に無いものを :save が書くと、書き出したファイルがエラーになる。
        assert_eq!(repl.repl.eng().sources(), ["def r1 := 1"]);
        assert_eq!(repl.repl.eng().unplayed(), ["def r2 := 2", "def r3 := 3"]);
    }

    /// ja: :restart をもう一度実行すると、保留にした宣言を再実行する
    /// en: Replays the pending declarations when :restart runs again
    #[story("F1", "F2")]
    #[test]
    fn replays_the_pending_declarations_when_restart_runs_again() {
        let mut repl = Driver::new();
        for one in ["def s1 := 1", "def s2 := 2"] {
            repl.feed(&[one]);
        }

        let mut hit = false;
        repl.hook(move |stage| match stage {
            Stage::Before(obj) if !hit && cmd(obj).starts_with("def s2") => {
                hit = true;
                Hooked::Kill
            }
            _ => Hooked::Pass,
        });
        repl.feed(&[":restart"]);
        repl.unhook(); // エンジンを直してから再実行する

        let out = repl.feed(&[":restart"]);
        assert!(out.contains("宣言 2 件"), "{out}");
        assert!(repl.feed(&["#check @s2"]).contains("s2 : Nat"));
        assert!(repl.repl.eng().unplayed().is_empty());
    }

    /// ja: 異常終了のあとの再実行を Ctrl-C で中断すると、入力した行を実行せず、そのことを表示する
    /// en: Skips the entered line and says so when the replay after a crash is interrupted with Ctrl-C
    #[story("A4", "F1")]
    #[test]
    fn skips_the_entered_line_and_says_so_when_the_replay_after_a_crash_is_interrupted_with_ctrl_c()
    {
        // ユーザーは再実行を止めたいので、入力した行も送らない。送ると、止めた
        // はずのエンジンに書いて「再実行しても異常終了した」と表示し、もう一度
        // 再起動する。
        let mut repl = Driver::new();
        for one in ["def r1 := 1", "def r2 := 2", "def r3 := 3"] {
            repl.feed(&[one]);
        }

        let (mut died, mut hit) = (false, false);
        repl.hook(move |stage| match stage {
            Stage::Before(obj) if !died && cmd(obj).starts_with("def r4") => {
                died = true;
                Hooked::Kill
            }
            Stage::Written(obj) if died && !hit && cmd(obj).starts_with("def r2") => {
                hit = true;
                Hooked::Interrupt
            }
            _ => Hooked::Pass,
        });
        let out = repl.feed(&["def r4 := 4"]);
        repl.unhook();

        assert!(out.contains("再実行を中断した"), "{out}");
        assert!(out.contains("入力した行は実行しなかった"), "{out}");
        assert!(!out.contains("再実行しても"), "{out}");
        assert_eq!(out.matches("件を再実行した").count(), 1, "{out}");
        assert_eq!(repl.declarations(), ["def r1 := 1"]);
        assert_eq!(repl.repl.eng().unplayed(), ["def r2 := 2", "def r3 := 3"]);
    }
}

mod when_an_internal_error_occurs {
    //! ja: 内部エラーが起きたとき
    //! en: When an internal error occurs
    //!
    //! 想定外の panic は「内部エラー」として表示し、セッションを続ける。
    use super::*;

    /// ja: 宣言を送る途中で leani が panic すると、内部エラーと表示して前の宣言を再実行する
    /// en: Shows an internal error and replays the earlier declarations when leani panics while sending a declaration
    #[story("F1")]
    #[test]
    fn shows_an_internal_error_and_replays_the_earlier_declarations_when_leani_panics_while_sending_a_declaration()
     {
        let mut repl = Driver::new();
        repl.feed(&["def before := 5"]);
        repl.hook(|stage| match stage {
            Stage::Before(obj) if cmd(obj).starts_with("def boom") => {
                panic!("わざと panic した")
            }
            _ => Hooked::Pass,
        });
        let out = repl.feed_with("feed_line", |r| {
            r.feed_line("def boom := 1");
        });
        assert!(out.contains("内部エラー: わざと panic した"), "{out}");
        assert!(out.contains("1 件を再実行"), "{out}");
        assert_eq!(repl.declarations(), ["def before := 5"]);
        assert!(repl.feed(&["before"]).contains('5'));
    }
}

/// テストの一時ディレクトリにファイルを置いて、そのパスを返す。
fn file(repl: &Driver, name: &str, text: &str) -> String {
    let path = repl.tmp().join(name);
    std::fs::write(&path, text).expect("write");
    path.display().to_string()
}

/// init ファイルを置いて実行する。
fn init_with(repl: &mut Driver, text: &str) -> String {
    let path = file(repl, "init.lean", text);
    repl.repl.set_init(&path);
    repl.feed_with("apply_init", |r| r.apply_init());
    path
}

/// core 以外の環境の boot を失敗させる。import が成功するかは
/// boot するまで分からないので、wide の起点の環境を作る要求でエンジンを止める。
/// 差し込み口は `:env` が新しいエンジンに引き継ぐ。
fn only_core(repl: &mut Driver) {
    repl.hook(|stage| match stage {
        Stage::Before(obj) if fresh(obj) && cmd(obj).contains("Lean.Elab.Frontend") => Hooked::Kill,
        _ => Hooked::Pass,
    });
}

/// どの環境の boot も失敗させる。
fn no_boot(repl: &mut Driver) {
    repl.hook(|stage| match stage {
        Stage::Before(obj) if fresh(obj) => Hooked::Kill,
        _ => Hooked::Pass,
    });
}

/// 設定ファイルに環境を 1 つ書き足す。
fn add_env(repl: &Driver, text: &str) {
    let config = repl.tmp().join("config.toml");
    let mut conf = std::fs::read_to_string(&config).expect("read config");
    conf.push_str(text);
    std::fs::write(&config, conf).expect("write config");
}

mod reading_and_writing_files {
    //! ja: ファイルの読み書き
    //! en: Reading and writing files
    use super::*;

    /// ja: ファイルを指定して leani を起動すると、そのファイルの宣言を読み込む
    /// en: Loads the declarations of a file when leani starts with the file
    #[story("B4")]
    #[test]
    fn loads_the_declarations_of_a_file_when_leani_starts_with_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("preload.lean");
        std::fs::write(&path, "def preloaded := 11\n").expect("write");
        let mut driver = Driver::preload(&path.display().to_string());
        assert!(driver.feed(&["preloaded"]).contains("11"));
    }

    /// ja: :save で書き出したファイルを :l で読み込むと、書き出した宣言を戻す
    /// en: Restores the saved declarations when a file written by :save is loaded with :l
    #[story("B3", "B4")]
    #[test]
    fn restores_the_saved_declarations_when_a_file_written_by_save_is_loaded_with_l() {
        let mut repl = Driver::new();
        let path = repl.tmp().join("saved.lean").display().to_string();
        repl.feed(&["def saved := 7"]);
        assert!(repl.feed(&[&format!(":save {path}")]).contains(&path));
        assert!(
            std::fs::read_to_string(&path)
                .expect("read")
                .contains("def saved := 7")
        );
        repl.feed(&[":reset"]);
        repl.feed(&[&format!(":l {path}")]);
        assert!(repl.feed(&["saved"]).contains('7'));
    }

    /// ja: エラーのあるファイルを :l で読み込むと、読み込めなかったと表示し、今の環境を残す
    /// en: Says the file could not be loaded and keeps the environment when :l loads a file with an error
    #[story("B4", "F1")]
    #[test]
    fn says_the_file_could_not_be_loaded_and_keeps_the_environment_when_l_loads_a_file_with_an_error()
     {
        // env を捨てたまま戻さないと、以後の入力が import 無しの環境に送られて
        // 何を書いてもエラーになる。
        let mut repl = Driver::new();
        let broken = file(&repl, "broken.lean", "def broken : Nat := \"oops\"\n");
        repl.feed(&["def before := 5"]);
        assert!(
            repl.feed(&[&format!(":l {broken}")])
                .contains("読み込めなかった")
        );
        assert!(repl.feed(&["before"]).contains('5'));
    }

    /// ja: 読む権限の無いファイルを :l で読み込むと、読めないと表示する
    /// en: Says the file cannot be read when :l loads a file without read permission
    #[story("B4")]
    #[test]
    fn says_the_file_cannot_be_read_when_l_loads_a_file_without_read_permission() {
        use std::os::unix::fs::PermissionsExt;
        let mut repl = Driver::new();
        let path = file(&repl, "locked.lean", "def x := 1\n");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        assert!(repl.feed(&[&format!(":l {path}")]).contains("読めない"));
    }

    /// ja: :l で読み込んだファイルを編集してから :r を実行すると、編集後の宣言を読み込む
    /// en: Loads the edited declarations when :r runs after the loaded file is edited
    #[story("B4")]
    #[test]
    fn loads_the_edited_declarations_when_r_runs_after_the_loaded_file_is_edited() {
        let mut repl = Driver::new();
        let path = file(&repl, "edited.lean", "def edited := 1\n");
        repl.feed(&[&format!(":l {path}")]);
        std::fs::write(&path, "def edited := 2\n").expect("write");
        repl.feed(&[":r"]);
        let out = repl.feed(&["edited"]);
        assert!(out.contains('2'), "{out}");
    }

    /// ja: ファイルを読み込んでいないときに :r を実行すると、入力した宣言をすべて消す
    /// en: Removes all entered declarations when :r runs with no file loaded
    #[story("B4", "F2")]
    #[test]
    fn removes_all_entered_declarations_when_r_runs_with_no_file_loaded() {
        let mut repl = Driver::new();
        repl.feed(&["def gone := 1"]);
        repl.feed(&[":r"]);
        assert!(repl.declarations().is_empty());
    }
}

mod building_up_declarations {
    //! ja: 宣言の組み立て
    //! en: Building up declarations
    //!
    //! 前に実行した宣言を次の宣言から呼べる。REPL 上で少しずつ組み立てる。
    use super::*;

    /// ja: 前の宣言を呼ぶ宣言を評価すると、その値を表示する
    /// en: Shows the value when a declaration that calls an earlier one is evaluated
    #[story("B2")]
    #[test]
    fn shows_the_value_when_a_declaration_that_calls_an_earlier_one_is_evaluated() {
        let mut repl = Driver::new();
        repl.feed(&["def step1 (n : Nat) := n * 2"]);
        repl.feed(&["def step2 (n : Nat) := step1 n + 1"]);
        assert!(repl.feed(&["#eval step2 5"]).contains("11"));
    }

    /// ja: :save を実行すると、宣言を実行した順に書き出す
    /// en: Writes the declarations in the order they ran when :save runs
    #[story("B2", "B3")]
    #[test]
    fn writes_the_declarations_in_the_order_they_ran_when_save_runs() {
        let mut repl = Driver::new();
        let path = repl.tmp().join("grown.lean").display().to_string();
        repl.feed(&["def step1 (n : Nat) := n * 2"]);
        repl.feed(&["def step2 (n : Nat) := step1 n + 1"]);
        repl.feed(&[&format!(":save {path}")]);

        let text = std::fs::read_to_string(&path).expect("read");
        // 順番が入れ替わると lean でエラーになる (Lean は前方参照を許さない)。
        assert!(text.find("def step1") < text.find("def step2"), "{text}");
    }
}

mod the_command_list {
    //! ja: コマンドの一覧
    //! en: The command list
    use super::*;

    /// ja: :help を実行すると、コマンドの一覧を表示する
    /// en: Shows the command list when :help runs
    #[story("H1")]
    #[test]
    fn shows_the_command_list_when_help_runs() {
        let mut repl = Driver::new();
        let out = repl.feed(&[":help"]);
        assert!(out.contains(":prove"), "{out}");
        assert!(out.contains(":env"), "{out}");
    }

    /// ja: 証明モードで :help を実行すると、証明モードのヘルプを表示する
    /// en: Shows the proof mode help when :help runs in proof mode
    #[story("H1")]
    #[test]
    fn shows_the_proof_mode_help_when_help_runs_in_proof_mode() {
        let mut repl = Driver::new();
        repl.feed(&["theorem t5 : 1 = 1 := by sorry", ":prove"]);
        assert!(repl.feed(&[":help"]).contains("1 回の入力が 1 タクティク"));
    }

    /// ja: 無いコマンドを入力すると、不明なコマンドと表示してセッションを続ける
    /// en: Says the command is unknown and keeps the session when a nonexistent command is entered
    #[story("H1")]
    #[test]
    fn says_the_command_is_unknown_and_keeps_the_session_when_a_nonexistent_command_is_entered() {
        let mut repl = Driver::new();
        assert!(repl.feed(&[":nosuch"]).contains("不明なコマンド: :nosuch"));
        assert!(repl.feed(&["1 + 1"]).contains('2'));
    }

    /// ja: :abbrev に記号か略記の先頭を渡すと、合う略記を表示し、無ければ無いと表示する
    /// en: Shows matching abbreviations for a symbol or a prefix, or says there are none, when :abbrev runs
    #[story("C7")]
    #[test]
    fn shows_matching_abbreviations_for_a_symbol_or_a_prefix_or_says_there_are_none_when_abbrev_runs()
     {
        let mut repl = Driver::new();
        assert!(repl.feed(&[":abbrev →"]).contains("\\to"));
        assert!(
            repl.feed(&[":abbrev nosuch"])
                .contains("nosuch で始まる略記は無い")
        );
        assert!(
            repl.feed(&[":abbrev あ"])
                .contains("あ を入力する略記は無い")
        );
    }
}

mod run_time {
    //! ja: 実行時間
    //! en: Run time
    //!
    //! 重い計算を切り分けるための目安。デフォルトでは表示しない (ノイズになる)。
    use super::*;

    /// ja: :time を実行するたびに、結果に実行時間を添えるかどうかを切り替える
    /// en: Toggles whether results show the run time each time :time runs
    #[story("A3")]
    #[test]
    fn toggles_whether_results_show_the_run_time_each_time_time_runs() {
        let mut repl = Driver::new();
        assert!(repl.feed(&[":time"]).contains("on"));
        let out = repl.feed(&["1 + 1"]);
        assert!(out.contains('2'), "{out}");
        assert!(out.contains("s)"), "{out}");

        assert!(repl.feed(&[":time"]).contains("off"));
        assert!(!repl.feed(&["1 + 1"]).contains("s)"));
    }
}

mod types_and_docstrings {
    //! ja: 型と docstring
    //! en: Types and docstrings
    use super::*;

    /// ja: :i に名前を渡すと、その型と docstring を表示する
    /// en: Shows the type and the docstring when :i runs with a name
    #[story("D2")]
    #[test]
    fn shows_the_type_and_the_docstring_when_i_runs_with_a_name() {
        let mut repl = Driver::new();
        let out = repl.feed(&[":i Nat.succ"]);
        assert!(out.contains("Nat.succ : Nat → Nat"), "{out}");
        assert!(out.contains("successor"), "{out}");
    }

    /// ja: :i に無い名前を渡すと、その名前が不明だと表示する
    /// en: Says the name is unknown when :i runs with a nonexistent name
    #[story("D2")]
    #[test]
    fn says_the_name_is_unknown_when_i_runs_with_a_nonexistent_name() {
        let mut repl = Driver::new();
        let out = repl.feed(&[":i Nat.nosuchThing"]);
        assert!(out.contains("不明: Nat.nosuchThing"), "{out}");
    }
}

mod showing_types_and_definitions {
    //! ja: 型と定義の表示
    //! en: Showing types and definitions
    use super::*;

    /// ja: :t か :type に式を渡すと、その式の型を表示する
    /// en: Shows the type of the expression when :t or :type runs with an expression
    #[story("D2")]
    #[test]
    fn shows_the_type_of_the_expression_when_t_or_type_runs_with_an_expression() {
        let mut repl = Driver::new();
        for cmd in [":t", ":type"] {
            assert!(repl.feed(&[&format!("{cmd} 1 + 1")]).contains("Nat"));
        }
    }

    /// ja: :p に定義の名前を渡すと、定義の本体を表示する
    /// en: Shows the body of the definition when :p runs with its name
    #[story("D3")]
    #[test]
    fn shows_the_body_of_the_definition_when_p_runs_with_its_name() {
        let mut repl = Driver::new();
        repl.feed(&["def shown := 1 + 2"]);
        assert!(repl.feed(&[":p shown"]).contains("1 + 2"));
    }

    /// ja: 名前を渡さずに :p を実行すると、名前が必要だと表示する
    /// en: Says a name is required when :p runs without a name
    #[story("D3", "H1")]
    #[test]
    fn says_a_name_is_required_when_p_runs_without_a_name() {
        let mut repl = Driver::new();
        assert!(repl.feed(&[":p"]).contains(":p には名前が必要"));
    }
}

mod theorem_search_with_an_external_service {
    //! ja: 外部サービスでの定理検索
    //! en: Theorem search with an external service
    //!
    //! :loogle は外部サービスに問い合わせるだけで、エンジンは一度も操作しない。
    use super::*;

    /// ja: パターンを渡さずに :loogle を実行すると、問い合わせずにパターンが必要だと表示する
    /// en: Says a pattern is required and sends no query when :loogle runs without a pattern
    #[story("D4")]
    #[test]
    fn says_a_pattern_is_required_and_sends_no_query_when_loogle_runs_without_a_pattern() {
        let mut repl = Driver::new();
        assert!(repl.feed(&[":loogle"]).contains("必要"));
        assert_eq!(repl.loogle_calls(), 0, "空の :loogle で問い合わせた");
    }

    /// ja: :loogle を実行すると、見つかった定理を表示し、環境を変えない
    /// en: Shows the found theorems and leaves the environment unchanged when :loogle runs
    #[story("D4")]
    #[test]
    fn shows_the_found_theorems_and_leaves_the_environment_unchanged_when_loogle_runs() {
        let mut repl = Driver::new();
        repl.feed(&["def beforeLoogle := 7"]);
        let env = repl.repl.eng().env();
        let hit = json!({
            "name": "Nat.add_comm",
            "type": " : ∀ (n m : Nat), n + m = m + n",
            "module": "Init",
        });
        repl.answer_loogle(Ok(json!({"count": 1, "hits": [hit]})));

        assert!(
            repl.feed(&[":loogle ?a + ?b = ?b + ?a"])
                .contains("Nat.add_comm")
        );
        // loogle が返すのは Mathlib の名前で、今の環境とは無関係。
        assert_eq!(repl.repl.eng().env(), env, ":loogle が環境を変えた");
        assert!(
            repl.feed(&["beforeLoogle"]).contains('7'),
            ":loogle が宣言を消した"
        );
    }

    /// ja: :loogle の問い合わせがタイムアウトすると、検索に失敗した理由を表示してセッションを続ける
    /// en: Shows why the search failed and keeps the session when the query for :loogle times out
    #[story("D4", "F4")]
    #[test]
    fn shows_why_the_search_failed_and_keeps_the_session_when_the_query_for_loogle_times_out() {
        let mut repl = Driver::new();
        repl.answer_loogle(Err("timed out"));

        let out = repl.feed(&[":loogle Nat"]);
        assert!(out.contains("検索に失敗した"), "{out}");
        assert!(out.contains("timed out"), "{out}");
        assert!(
            repl.feed(&["1 + 1"]).contains('2'),
            "loogle の失敗のあと式を評価できない"
        );
    }
}

mod safeguards_for_saving_files {
    //! ja: 書き出しの安全対策
    //! en: Safeguards for saving files
    //!
    //! :save は宣言を残す操作。そこで宣言を失わせない。
    use super::*;

    /// ja: すでにあるファイルに :save すると、すでにあると表示して上書きしない
    /// en: Says the file exists and does not overwrite it when :save targets an existing file
    #[story("B3")]
    #[test]
    fn says_the_file_exists_and_does_not_overwrite_it_when_save_targets_an_existing_file() {
        let mut repl = Driver::new();
        let path = file(&repl, "existing.lean", "-- 大事なもの\n");
        repl.feed(&["def x := 1"]);
        assert!(
            repl.feed(&[&format!(":save {path}")])
                .contains("すでにある")
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "-- 大事なもの\n"
        );
    }

    /// ja: リンク先の無いシンボリックリンクに :save すると、すでにあると表示してリンク先を作らない
    /// en: Says the file exists and does not create the target when :save targets a dangling symlink
    #[story("B3")]
    #[test]
    fn says_the_file_exists_and_does_not_create_the_target_when_save_targets_a_dangling_symlink() {
        // `exists` はリンク先を見るので、リンク先が無いと「無い」と判定する。そのまま
        // 書くと、リンク先に新しいファイルができる。
        let mut repl = Driver::new();
        let target = repl.tmp().join("gone.lean");
        let link = repl.tmp().join("dangling.lean");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        repl.feed(&["def x := 1"]);
        let path = link.display().to_string();
        assert!(
            repl.feed(&[&format!(":save {path}")])
                .contains("すでにある")
        );
        assert!(!target.exists());
    }

    /// ja: leani が書き出したファイルにもう一度 :save すると、そのファイルを上書きする
    /// en: Overwrites the file when :save targets a file that leani wrote
    #[story("B3")]
    #[test]
    fn overwrites_the_file_when_save_targets_a_file_that_leani_wrote() {
        let mut repl = Driver::new();
        let path = repl.tmp().join("mine.lean").display().to_string();
        repl.feed(&["def x := 1"]);
        repl.feed(&[&format!(":save {path}")]);
        repl.feed(&["def y := 2"]);
        assert!(repl.feed(&[&format!(":save {path}")]).contains(&path));
        assert!(
            std::fs::read_to_string(&path)
                .expect("read")
                .contains("def y := 2")
        );
    }

    /// ja: 無いディレクトリの中に :save すると、書き出せなかったと表示してセッションを続ける
    /// en: Says the file could not be written and keeps the session when :save targets a missing directory
    #[story("B3")]
    #[test]
    fn says_the_file_could_not_be_written_and_keeps_the_session_when_save_targets_a_missing_directory()
     {
        let mut repl = Driver::new();
        let path = repl.tmp().join("no/such/dir/x.lean").display().to_string();
        repl.feed(&["def x := 1"]);
        let out = repl.feed(&[&format!(":save {path}")]);
        assert!(out.contains("書き出せなかった"), "{out}");
        assert!(repl.feed(&["1 + 1"]).contains('2'));
    }
}

mod when_a_file_cannot_be_reloaded_after_a_restart {
    //! ja: 再起動のあとで読み込み直せないとき
    //! en: When a file cannot be reloaded after a restart
    use super::*;

    /// ja: 読み込んだファイルを消してから :restart を実行すると、読み込み直せなかったと表示してその宣言を外す
    /// en: Reports the failed reload and drops the file's declarations when the loaded file is deleted before :restart
    #[story("F1", "B4")]
    #[test]
    fn reports_the_failed_reload_and_drops_the_file_s_declarations_when_the_loaded_file_is_deleted_before_restart()
     {
        // 外さないと、環境に無い宣言を :save が書き出す。
        let mut repl = Driver::new();
        let path = file(&repl, "gone.lean", "def fromFile := 7\n");
        repl.feed(&[&format!(":l {path}")]);
        std::fs::remove_file(&path).expect("unlink");

        let out = repl.feed(&[":restart"]);
        assert!(out.contains("読み込み直せなかった"), "{out}");
        assert!(repl.repl.eng().loaded().is_none());
        assert!(!repl.repl.eng().sources().join("\n").contains("fromFile"));
    }

    /// ja: init を送る途中で再起動を中断すると、init を残し、次の :restart で init を戻す
    /// en: Keeps init and restores it on the next :restart when the restart is interrupted while sending init
    #[story("F1", "G2")]
    #[test]
    fn keeps_init_and_restores_it_on_the_next_restart_when_the_restart_is_interrupted_while_sending_init()
     {
        // init ファイルの内容に問題があるわけではないので、次の :restart で戻せる。
        let mut repl = Driver::new();
        init_with(&mut repl, "def fromInit := 99\n");
        let init_src = repl.repl.eng().init_src().expect("init_src").to_string();

        // init の送信を失敗させる。Kill はプロセスを止めるので、止めずに失敗させられる
        // Interrupt を使う。
        repl.hook(move |stage| match stage {
            Stage::Before(obj) if cmd(obj) == init_src => Hooked::Interrupt,
            _ => Hooked::Pass,
        });
        let out = repl.feed(&[":restart"]);
        // 再起動そのものを止めたので、宣言の再実行には進まない。
        assert!(out.contains("再起動を中断した"), "{out}");
        assert!(repl.repl.eng().init_src().is_some());

        repl.unhook();
        repl.feed(&[":restart"]);
        assert!(repl.feed(&["fromInit"]).contains("99"));
    }
}

mod handling_init {
    //! ja: init の扱い
    //! en: Handling init
    //!
    //! init は base の環境に含めるので、環境を作り直すと消えやすい。
    use super::*;

    /// ja: :restart を実行すると、init の宣言を残す
    /// en: Keeps the init declarations when :restart runs
    #[story("F1", "G2")]
    #[test]
    fn keeps_the_init_declarations_when_restart_runs() {
        let mut repl = Driver::new();
        init_with(&mut repl, "def fromInit := 99\n");

        assert!(repl.feed(&["fromInit"]).contains("99"));
        repl.feed(&[":restart"]);
        assert!(
            repl.feed(&["fromInit"]).contains("99"),
            ":restart で init が消えた"
        );
    }

    /// ja: :reset を実行すると、init の宣言を残す
    /// en: Keeps the init declarations when :reset runs
    #[story("F2", "G2")]
    #[test]
    fn keeps_the_init_declarations_when_reset_runs() {
        let mut repl = Driver::new();
        init_with(&mut repl, "def fromInit := 99\n");

        repl.feed(&[":reset"]);
        assert!(
            repl.feed(&["fromInit"]).contains("99"),
            ":reset で init が消えた"
        );
    }

    /// ja: init で名前空間を open すると、入力した式でその名前空間の短い名前を評価する
    /// en: Evaluates short names from a namespace in entered input when init opens the namespace
    #[story("G2")]
    #[test]
    fn evaluates_short_names_from_a_namespace_in_entered_input_when_init_opens_the_namespace() {
        let mut repl = Driver::new();
        init_with(&mut repl, "open Nat\n");

        assert!(repl.feed(&["succ 1"]).contains('2'));
    }

    /// ja: :reset を実行すると、init に書いた open を残す
    /// en: Keeps the open from init when :reset runs
    #[story("F2", "G2")]
    #[test]
    fn keeps_the_open_from_init_when_reset_runs() {
        let mut repl = Driver::new();
        init_with(&mut repl, "open Nat\n");

        repl.feed(&[":reset"]);
        assert!(repl.feed(&["succ 1"]).contains('2'));
    }

    /// ja: エラーのある init を読み込むと、エラーがあると表示して環境を変えない
    /// en: Reports the error and leaves the environment unchanged when an init file with an error is loaded
    #[story("G2", "G4")]
    #[test]
    fn reports_the_error_and_leaves_the_environment_unchanged_when_an_init_file_with_an_error_is_loaded()
     {
        let mut repl = Driver::new();
        let base = repl.repl.eng().base();
        let out = init_with(&mut repl, "def bad : Nat := \"x\"\n");
        let _ = out;
        let out = repl.feed_with("apply_init", |r| r.apply_init());
        assert!(out.contains("にエラーがある"), "{out}");
        assert_eq!(repl.repl.eng().base(), base);
        assert!(repl.repl.eng().init_src().is_none());
    }

    /// ja: init にディレクトリを指定して読み込むと、読めないと表示する
    /// en: Says the init file cannot be read when a directory is given as init
    #[story("G2", "G4")]
    #[test]
    fn says_the_init_file_cannot_be_read_when_a_directory_is_given_as_init() {
        let mut repl = Driver::new();
        let dir = repl.tmp().display().to_string(); // ディレクトリ
        repl.repl.set_init(&dir);
        let out = repl.feed_with("apply_init", |r| r.apply_init());
        assert!(out.contains("が読めない"), "{out}");
    }

    /// ja: import だけの init を読み込むと、何も表示せず環境を変えない
    /// en: Shows nothing and leaves the environment unchanged when an init file with only imports is loaded
    #[story("G2")]
    #[test]
    fn shows_nothing_and_leaves_the_environment_unchanged_when_an_init_file_with_only_imports_is_loaded()
     {
        let mut repl = Driver::new();
        let path = file(&repl, "init.lean", "import Lean\n");
        repl.repl.set_init(&path);
        let env = repl.repl.eng().env();
        let out = repl.feed_with("apply_init", |r| r.apply_init());
        assert_eq!(out, "");
        assert_eq!(repl.repl.eng().env(), env);
    }
}

mod declarations_included_in_a_saved_file {
    //! ja: 書き出しに含める宣言
    //! en: Declarations included in a saved file
    //!
    //! log だけ書き出すと、読み込み直したファイルが元の環境と違うものになる。
    use super::*;

    /// ja: :save を実行すると、init、読み込んだファイル、入力した宣言をすべて書き出す
    /// en: Writes the declarations from init, loaded files and input when :save runs
    #[story("B3", "G2")]
    #[test]
    fn writes_the_declarations_from_init_loaded_files_and_input_when_save_runs() {
        let mut repl = Driver::new();
        let loaded = file(&repl, "loaded.lean", "def fromFile := 2\n");
        let out = repl.tmp().join("all.lean").display().to_string();

        init_with(&mut repl, "def fromInit := 1\n");
        repl.feed(&[&format!(":l {loaded}")]);
        repl.feed(&["def typed := 3"]);
        repl.feed(&[&format!(":save {out}")]);

        let text = std::fs::read_to_string(&out).expect("read");
        for name in ["fromInit", "fromFile", "typed"] {
            assert!(text.contains(name), "{name} が書き出されていない:\n{text}");
        }

        repl.feed(&[":reset"]);
        repl.feed(&[&format!(":l {out}")]);
        assert!(repl.feed(&["fromInit"]).contains('1'));
        assert!(repl.feed(&["fromFile"]).contains('2'));
        assert!(repl.feed(&["typed"]).contains('3'));
    }
}

mod when_a_rewrite_is_abandoned {
    //! ja: 書き直しをやめたとき
    //! en: When a rewrite is abandoned
    //!
    //! 前の宣言の続きを書き始めた時点で、直前の宣言は取り消されている。
    use super::*;

    /// ja: 宣言の続きを入力している途中で Ctrl-C を押すと、元の宣言を戻す
    /// en: Restores the original declaration when Ctrl-C is pressed while continuing a declaration
    #[story("C2")]
    #[test]
    fn restores_the_original_declaration_when_ctrl_c_is_pressed_while_continuing_a_declaration() {
        let mut repl = Driver::new();
        repl.feed(&["def three := 1"]);
        repl.feed(&["  + 2"]); // 前の宣言の続きを書く (宣言は取り消されている)
        repl.feed_with("discard", Repl::discard); // Ctrl-C

        assert!(
            repl.feed(&["three"]).contains('1'),
            "取り消した宣言が戻っていない"
        );
        assert_eq!(repl.declarations().len(), 1);
    }

    /// ja: 宣言の続きを空行で確定したあとに Ctrl-C を押すと、書き直した宣言を残す
    /// en: Keeps the rewritten declaration when Ctrl-C is pressed after the continuation is confirmed
    #[story("C2")]
    #[test]
    fn keeps_the_rewritten_declaration_when_ctrl_c_is_pressed_after_the_continuation_is_confirmed()
    {
        let mut repl = Driver::new();
        repl.feed(&["def three := 1"]);
        repl.block(&["  + 2"]);
        repl.feed_with("discard", Repl::discard);
        assert!(
            repl.feed(&["three"]).contains('3'),
            "確定した書き直しが巻き戻された"
        );
        assert_eq!(repl.declarations().len(), 1);
    }

    /// ja: タクティクの続きを入力している途中で Ctrl-C を押すと、直前のタクティクを戻す
    /// en: Restores the previous tactic when Ctrl-C is pressed while continuing a tactic
    #[story("C2", "E1")]
    #[test]
    fn restores_the_previous_tactic_when_ctrl_c_is_pressed_while_continuing_a_tactic() {
        let mut repl = Driver::new();
        repl.feed(&["theorem keepTac : 1 = 1 ∧ 2 = 2 := by sorry", ":prove"]);
        repl.feed(&["constructor"]);
        repl.feed(&["  <;> rfl"]); // 前のタクティクの続きを書く (タクティクは取り消されている)
        repl.feed_with("discard", Repl::discard); // Ctrl-C

        assert!(
            !repl.feed(&[":goals"]).contains('∧'),
            "取り消したタクティクが戻っていない"
        );
        assert!(repl.feed(&[":script"]).contains("constructor"));
    }

    /// ja: タクティクの続きで証明が終わったあとに Ctrl-C を押すと、完成した証明を残す
    /// en: Keeps the finished proof when Ctrl-C is pressed after a tactic continuation finishes it
    #[story("C2", "E1")]
    #[test]
    fn keeps_the_finished_proof_when_ctrl_c_is_pressed_after_a_tactic_continuation_finishes_it() {
        let mut repl = Driver::new();
        repl.feed(&["theorem doneTac : 1 = 1 ∧ 2 = 2 := by sorry", ":prove"]);
        repl.feed(&["constructor"]);
        assert!(repl.block(&["  <;> rfl"]).contains("証明完了"));
        repl.feed_with("discard", Repl::discard);

        assert!(
            repl.feed(&[":goals"]).contains("証明モードではない"),
            "確定した書き直しが巻き戻された"
        );
        let decl = repl.declarations().pop().expect("宣言が無い");
        assert!(!decl.contains("sorry"), "{decl}");
    }

    /// ja: sorry を含む宣言の続きを入力している途中で Ctrl-C を押すと、:prove でその sorry のゴールを表示する
    /// en: Shows the sorry's goal on :prove when Ctrl-C is pressed while continuing a declaration with sorry
    #[story("C2", "E1")]
    #[test]
    fn shows_the_sorry_s_goal_on_prove_when_ctrl_c_is_pressed_while_continuing_a_declaration_with_sorry()
     {
        let mut repl = Driver::new();
        repl.feed(&["def withSorry : Nat := sorry"]);
        // 続きを書いているあいだは宣言が取り消されているので、保留した sorry も
        // 今の環境のものではない (Driver が 1 行ごとに確かめる)。
        repl.feed(&["  + 1"]);
        repl.feed_with("discard", Repl::discard); // Ctrl-C

        assert!(
            repl.feed(&[":prove"]).contains('⊢'),
            "取り消した宣言の sorry が戻っていない"
        );
    }
}

mod interrupting_a_load {
    //! ja: 読み込みの中断
    //! en: Interrupting a load
    use super::*;

    /// ja: ファイルの読み込みを中断すると、読み込む前の環境と宣言を残す
    /// en: Keeps the previous environment and declarations when loading a file is interrupted
    #[story("A4", "F1")]
    #[test]
    fn keeps_the_previous_environment_and_declarations_when_loading_a_file_is_interrupted() {
        let mut repl = Driver::new();
        repl.feed(&["def before := 5"]);
        let path = repl.tmp().join("any.lean").display().to_string();
        let keep = (
            repl.repl.eng().env(),
            repl.repl.eng().stack().to_vec(),
            repl.repl.eng().log().to_vec(),
        );

        interrupt_after_write(&mut repl, |o| cmd(o).contains("loaded"));
        let r = repl
            .repl
            .eng_mut()
            .load_file(&path, Some("def loaded := 1\n"));
        assert!(matches!(r, Err(EngineFault::Interrupted)), "{r:?}");
        let eng = repl.repl.eng();
        assert_eq!((eng.env(), eng.stack().to_vec(), eng.log().to_vec()), keep);

        repl.unhook();
        assert!(repl.feed(&["before"]).contains('5'));
    }

    /// ja: :l で init を適用し直す途中で中断すると、ファイルを読み込まずに前の宣言を残す
    /// en: Cancels the load and keeps earlier declarations when reapplying init during :l is interrupted
    #[story("A4", "F1", "G2")]
    #[test]
    fn cancels_the_load_and_keeps_earlier_declarations_when_reapplying_init_during_l_is_interrupted()
     {
        // ファイルの読み込みは終わっていても、init を戻せていない環境は使わせない。
        // 「中断した」と表示したのに読み込んだままにすると、表示と状態が食い違い、
        // それまでの宣言も表示なしに消える。
        let mut repl = Driver::new();
        init_with(&mut repl, "def fromInit := 99\n");
        let init_src = repl.repl.eng().init_src().expect("init_src").to_string();
        repl.feed(&["def before := 5"]);
        let path = file(&repl, "loaded.lean", "def loaded := 1\n");

        let hit = interrupt_after_write(&mut repl, move |o| cmd(o) == init_src);
        repl.feed(&[&format!(":l {path}")]);
        assert!(hit.load(SeqCst), "init の再適用を止めていない");
        repl.unhook();

        assert_eq!(repl.repl.eng().loaded(), None);
        assert_eq!(repl.declarations(), ["def before := 5"]);
        assert!(repl.feed(&["before"]).contains('5'));
    }
}

mod writing_history {
    //! ja: 履歴の書き出し
    //! en: Writing history
    //!
    //! 履歴は確定した入力を 1 件として、そのつど追記する。ファイル全体を書き直さないので、
    //! 読めない形式のファイルがあっても消さない。
    use super::*;
    use leani::history::BlockHistory;

    /// ja: 読めない形式の履歴ファイルに入力を記録すると、前の内容を残す
    /// en: Keeps the existing content when an input is recorded in a history file in an unreadable format
    #[story("F3")]
    #[test]
    fn keeps_the_existing_content_when_an_input_is_recorded_in_a_history_file_in_an_unreadable_format()
     {
        // readline や libedit の履歴を引き継いだ環境ではこの形式になる。読み込むときは
        // 無視されるが、書き込みは追記なので前の履歴は残る。
        let dir = tempfile::tempdir().expect("tempdir");
        let hist = dir.path().join("gnu-history");
        std::fs::write(&hist, "1 + 1\n2 + 2\n").expect("write");

        BlockHistory::new(&hist).record("3 + 3");
        let text = std::fs::read_to_string(&hist).expect("read");
        assert!(text.starts_with("1 + 1\n2 + 2\n"), "前の履歴が消えた");
    }

    /// ja: 複数行の入力を記録すると、読み込んだときに 1 件として返す
    /// en: Loads a multi-line input back as one entry when it is recorded
    #[story("C3", "F3")]
    #[test]
    fn loads_a_multi_line_input_back_as_one_entry_when_it_is_recorded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let history = BlockHistory::new(dir.path().join("history"));
        history.record("def f : Nat -> Nat\n  | 0 => 1");
        assert_eq!(history.load(), ["def f : Nat -> Nat\n  | 0 => 1"]);
    }

    /// ja: leani を起動すると、標準入力が端末のときだけ履歴を使う
    /// en: Enables history at startup only if stdin is a terminal
    #[story("F3")]
    #[test]
    fn enables_history_at_startup_only_if_stdin_is_a_terminal() {
        // echo … | leani を端末で実行したとき。stdout は端末だが、パイプから読んだ
        // 入力を履歴に残さない。
        let has_history = |stdin_tty: bool| {
            let mut sb = sandbox();
            sb.ctx.tty = true;
            sb.ctx.stdin_tty = stdin_tty;
            let table = load_config(&sb.ctx.config).expect("config");
            let cfg = resolve(&sb.ctx, None, None, &[], &table).expect("resolve");
            let mut repl = Repl::new(
                &sb.ctx,
                cfg,
                None,
                Box::new(Sink::default()),
                Box::new(FakeSearch::default()),
            )
            .expect("起動できない");
            let got = repl.history().is_some();
            repl.close();
            got
        };
        assert!(has_history(true));
        assert!(!has_history(false));
    }

    /// ja: 行編集が 1 行を履歴に追加しようとすると、その行を記録しない
    /// en: Records nothing when line editing tries to add a single line to history
    #[story("C3", "F3")]
    #[test]
    fn records_nothing_when_line_editing_tries_to_add_a_single_line_to_history() {
        // 行編集の crate は 1 行読むたびに 1 行追加しようとする。受け付けると
        // 複数行の宣言が行ごとに分かれ、呼び戻すのに Ctrl-P が何度も必要になる。
        let dir = tempfile::tempdir().expect("tempdir");
        let history = BlockHistory::new(dir.path().join("history"));
        history.append_string("  | 0 => 1");
        assert!(history.load().is_empty());
    }
}

mod input_recalled_from_history {
    //! ja: 履歴から呼び出した入力
    //! en: Input recalled from history
    //!
    //! 履歴から呼び出した複数行は、改行を含む 1 行として `Repl::feed_line` に渡される。
    use super::*;

    /// ja: 完結したブロックで Enter を押すと、空行を待たずに実行する
    /// en: Runs a complete block without waiting for a blank line when Enter is pressed
    #[story("C3")]
    #[test]
    fn runs_a_complete_block_without_waiting_for_a_blank_line_when_enter_is_pressed() {
        let mut repl = Driver::new();
        repl.feed_with("feed_line", |r| {
            r.feed_line("def recalled : Nat -> Nat\n  | 0 => 1\n  | _ => 2");
        });
        assert!(
            repl.declarations()
                .last()
                .expect("宣言")
                .contains("def recalled")
        );
        assert!(repl.repl.buf().is_empty());
    }

    /// ja: 入力途中のブロックで Enter を押すと、送らずに続きの行を待つ
    /// en: Waits for more lines without sending when Enter is pressed on an unfinished block
    #[story("C3")]
    #[test]
    fn waits_for_more_lines_without_sending_when_enter_is_pressed_on_an_unfinished_block() {
        // Ctrl-C で捨てた入力も、途中のまま履歴に残る。呼び出してすぐに送ると
        // unexpected end of input のエラーになる。
        let mut repl = Driver::new();
        repl.feed_with("feed_line", |r| {
            r.feed_line("#eval do\n  let x := 1\n  for i in [1, 2] do");
        });
        // 送っていなければ、入力はバッファに残っている。
        assert!(!repl.repl.buf().is_empty());
        let out = repl.feed(&["    IO.println (x + i)", ""]);
        assert!(out.contains("2\n3"), "{out}");
    }
}

mod what_the_replay_could_not_run {
    //! ja: replay で再実行できなかったもの
    //! en: What the replay could not run
    //!
    //! 何も表示せずに消えると、ユーザーが気付く場所が無い。
    use super::*;

    /// ja: 読み込んだファイルにエラーを入れて :restart を実行すると、読み込み直せなかったことと消えた宣言を表示する
    /// en: Reports the failed reload and the lost declarations when :restart runs after the loaded file gets an error
    #[story("F1", "B4")]
    #[test]
    fn reports_the_failed_reload_and_the_lost_declarations_when_restart_runs_after_the_loaded_file_gets_an_error()
     {
        let mut repl = Driver::new();
        let path = file(&repl, "lib.lean", "def libA := 10\n");
        repl.feed(&[&format!(":l {path}")]);
        repl.feed(&["def usesA := libA + 1"]);

        std::fs::write(&path, "def libA := (10 : Nat) +\n").expect("write"); // 直せない形に書き換える
        let out = repl.feed(&[":restart"]);

        assert!(out.contains("読み込み直せなかった"), "{out}");
        assert!(out.contains("def usesA"), "{out}");
        // env に無いものを sources() が並べ続けると、:save したファイルが
        // :l で「すでに宣言されている」というエラーになる。
        assert!(
            repl.repl.eng().sources().is_empty(),
            "{:?}",
            repl.repl.eng().sources()
        );
    }
}

mod header_of_a_saved_file {
    //! ja: 書き出したファイルのヘッダ
    //! en: Header of a saved file
    use super::*;

    /// ja: :save を実行すると、ファイルの先頭に import Lean を書く
    /// en: Writes import Lean at the top of the file when :save runs
    #[story("B3")]
    #[test]
    fn writes_import_lean_at_the_top_of_the_file_when_save_runs() {
        let mut repl = Driver::new();
        let path = repl.tmp().join("out.lean").display().to_string();
        repl.feed(&["def savedOne := Lean.versionString"]);
        repl.feed(&[&format!(":save {path}")]);

        // :l は読み込むときに import Lean を足すので、:l では問題が見つからない。
        // lean に直接渡したときだけ Lean.versionString が見つからなくなる。
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.starts_with("import Lean\n"), "{text}");
    }
}

mod when_init_and_a_loaded_file_are_combined {
    //! ja: init と読み込みが混ざるとき
    //! en: When init and a loaded file are combined
    use super::*;

    /// ja: :l でファイルを読み込むと、ファイルのあとに init の宣言を追加し直す
    /// en: Adds the init declarations again after the file when :l loads a file
    #[story("G2", "B3")]
    #[test]
    fn adds_the_init_declarations_again_after_the_file_when_l_loads_a_file() {
        let mut repl = Driver::new();
        init_with(&mut repl, "def fromInit := 1\n");

        let lib = file(&repl, "lib.lean", "def fromFile := 2\n");
        repl.feed(&[&format!(":l {lib}")]);
        repl.feed(&["def fromRepl := 3"]);

        // :l は環境を作り直す。init を追加し直さないと、env には無いものを
        // sources() が並べ続ける。並べる順も実行した順でないとエラーになる。
        assert_eq!(
            repl.repl.eng().sources(),
            [
                "def fromFile := 2",
                "def fromInit := 1",
                "def fromRepl := 3"
            ]
        );
        assert!(
            repl.feed(&["#eval fromInit + fromFile + fromRepl"])
                .contains('6')
        );
    }
}

mod undoing_declarations {
    //! ja: 宣言の取り消し
    //! en: Undoing declarations
    use super::*;

    /// ja: :undo n を実行すると、最後の n 件の宣言を取り消す
    /// en: Undoes the last n declarations when :undo n runs
    #[story("F2")]
    #[test]
    fn undoes_the_last_n_declarations_when_undo_n_runs() {
        let mut repl = Driver::new();
        repl.feed(&["def u1 := 1"]);
        repl.feed(&["def u2 := 2"]);
        repl.feed(&["def u3 := 3"]);
        repl.feed(&[":undo 2"]);
        assert_eq!(repl.declarations(), ["def u1 := 1"]);
        assert!(repl.feed(&["u1"]).contains('1'));
    }
}

mod pending_declarations_and_engine_generations {
    //! ja: 保留した宣言とエンジンの世代
    //! en: Pending declarations and engine generations
    //!
    //! 取り消した宣言の中身は env id を持つ。再起動したエンジンにその id は無い。
    use super::*;

    /// ja: 書き直しの途中で :restart してから書き直しをやめると、取り消した宣言を使う式を評価する
    /// en: Evaluates an expression using the undone declaration when rewriting is abandoned after :restart
    #[story("F1", "C2")]
    #[test]
    fn evaluates_an_expression_using_the_undone_declaration_when_rewriting_is_abandoned_after_restart()
     {
        let mut repl = Driver::new();
        repl.feed(&["def held := 1"]);
        // 前の宣言を書き直している途中
        let undone = repl.repl.eng_mut().pop_decl();
        repl.repl.set_undone(undone);
        repl.feed(&[":restart"]);

        repl.feed_with("restore_undone", Repl::restore_undone); // 書き直さずにやめた
        // 無効になった env id を設定すると repl は "Unknown environment." しか返さず、
        // 何を入力しても反応しない端末になる。テキストから再実行する。
        assert!(repl.feed(&["#eval held + 1"]).contains('2'));
    }

    /// ja: 書き直し中にエンジンが異常終了してからやめると、前の宣言に sorry があっても :prove は sorry が無いと表示する
    /// en: Says :prove finds no sorry when a rewrite after a declaration with sorry is abandoned after an engine crash
    #[story("C2", "E1", "F1")]
    #[test]
    fn says_prove_finds_no_sorry_when_a_rewrite_after_a_declaration_with_sorry_is_abandoned_after_an_engine_crash()
     {
        let mut repl = Driver::new();
        repl.feed(&["theorem s1 : True := by sorry", "def other := 1"]);

        // other の続きを書き始めると other は取り消され、完結判定の途中で
        // エンジンが終了する。再起動の replay で s1 の sorry を保留し直す。
        die_once(&mut repl, |o| o.get("cmd").is_some());
        repl.feed(&["  + 1"]);
        repl.unhook();
        // Ctrl-C で other をテキストから再実行する。直前の宣言は other なので、
        // s1 の sorry を保留したままにすると、:prove が other の環境で s1 の
        // 証明を始める。
        repl.feed_with("discard", Repl::discard);

        assert!(repl.feed(&[":prove"]).contains("sorry が無い"));
        assert!(repl.feed(&["other"]).contains('1'));
    }

    /// ja: エンジンが無効な env id を持つときに宣言を入力すると、環境のエラーと :restart を表示する
    /// en: Shows an environment error and :restart when a declaration is entered and the engine has an invalid env id
    #[story("F1")]
    #[test]
    fn shows_an_environment_error_and_restart_when_a_declaration_is_entered_and_the_engine_has_an_invalid_env_id()
     {
        let mut repl = Driver::new();
        repl.repl.eng_mut().set_env(Some(987_654)); // 無効になった世代の env id を持つ状態
        let out = repl.feed(&["def afterGhost := 1"]);

        assert!(out.to_lowercase().contains("environment"), "{out}");
        assert!(out.contains(":restart"), "{out}");
    }
}

mod declarations_missing_from_the_environment {
    //! ja: 環境に無い宣言の扱い
    //! en: Declarations missing from the environment
    //!
    //! replay でエラーになった宣言と実行できなかった宣言は env に無い。テキストは保留
    //! しておくが、そのまま書き出すとエラーになるファイルができる。
    use super::*;

    /// ja: 依存先が消えたあとで :restart すると、再実行に失敗した宣言を表示して保留にする
    /// en: Shows and keeps pending a declaration that fails to replay when :restart runs without its dependency
    #[story("F1")]
    #[test]
    fn shows_and_keeps_pending_a_declaration_that_fails_to_replay_when_restart_runs_without_its_dependency()
     {
        let mut repl = Driver::new();
        let path = file(&repl, "lib2.lean", "def libB := 10\n");
        repl.feed(&[&format!(":l {path}")]);
        repl.feed(&["def usesB := libB + 1"]);

        std::fs::write(&path, "def other := 1\n").expect("write"); // libB を消す
        let out = repl.feed(&[":restart"]);

        assert!(out.contains("再実行に失敗した宣言"), "{out}");
        assert_eq!(repl.repl.eng().unplayed(), ["def usesB := libB + 1"]);
    }

    /// ja: :save すると、保留にした宣言をコメントとして書き出す
    /// en: Writes pending declarations as comments when :save runs
    #[story("B3", "F1")]
    #[test]
    fn writes_pending_declarations_as_comments_when_save_runs() {
        let mut repl = Driver::new();
        let path = file(&repl, "lib3.lean", "def libC := 10\n");
        repl.feed(&[&format!(":l {path}")]);
        repl.feed(&["def usesC := libC + 1"]);
        std::fs::write(&path, "def other := 1\n").expect("write");
        repl.feed(&[":restart"]);

        let out_path = repl.tmp().join("orphan.lean").display().to_string();
        repl.feed(&[&format!(":save {out_path}")]);
        let text = std::fs::read_to_string(&out_path).expect("read");

        // 保留した宣言を書き出さないと、:save は成功を報告したのに入力した宣言が消える。
        // そのまま書くと lean でエラーになる。
        assert!(text.contains("-- def usesC := libC + 1"), "{text}");
        assert!(text.contains("def other := 1"), "{text}");
    }

    /// ja: :reset すると、保留にした宣言を捨てたと表示し、そのあとの :restart で再実行しない
    /// en: Discards pending declarations with a notice when :reset runs and does not replay them on :restart
    #[story("F2")]
    #[test]
    fn discards_pending_declarations_with_a_notice_when_reset_runs_and_does_not_replay_them_on_restart()
     {
        let mut repl = Driver::new();
        let path = file(&repl, "lib4.lean", "def libD := 10\n");
        repl.feed(&[&format!(":l {path}")]);
        repl.feed(&["def usesD := libD + 1"]);
        std::fs::write(&path, "def other := 1\n").expect("write");
        repl.feed(&[":restart"]);

        let out = repl.feed(&[":reset"]);
        assert!(out.contains("捨てた"), "{out}");
        assert!(repl.repl.eng().unplayed().is_empty());

        // 残すと、:reset で消したはずの宣言が次の :restart で戻ってくる。
        repl.feed(&[":restart"]);
        assert!(!repl.feed(&["#check @usesD"]).contains("usesD : Nat"));
    }
}

mod imports_of_a_loaded_file {
    //! ja: 読み込んだファイルの import
    //! en: Imports of a loaded file
    use super::*;

    /// ja: :save すると、その import を書き出したファイルのヘッダに書く
    /// en: Writes the imports into the header of the saved file when :save runs
    #[story("B3", "B4")]
    #[test]
    fn writes_the_imports_into_the_header_of_the_saved_file_when_save_runs() {
        let mut repl = Driver::new();
        let src = file(
            &repl,
            "withimport.lean",
            "import Lean.Elab.Frontend\n\ndef needsFrontend := 1\n",
        );
        repl.feed(&[&format!(":l {src}")]);

        let out_path = repl.tmp().join("kept.lean").display().to_string();
        repl.feed(&[&format!(":save {out_path}")]);
        let text = std::fs::read_to_string(&out_path).expect("read");
        let head = text.split("\n\n").next().unwrap_or("");

        // import を書き戻さないと、:save は成功を報告するのにそのファイルは :l でも
        // lean でもエラーになる (Unknown identifier が並ぶ)。
        assert!(head.contains("import Lean.Elab.Frontend"), "{head}");
        assert!(head.contains("import Lean\n"), "{head}");
    }

    /// ja: :restart でファイルを読み込み直せなかったあとに :save すると、そのファイルの import を書かない
    /// en: Leaves out the imports of a file that :restart could not reload when :save runs
    #[story("B3", "F1")]
    #[test]
    fn leaves_out_the_imports_of_a_file_that_restart_could_not_reload_when_save_runs() {
        let mut repl = Driver::new();
        let src = file(
            &repl,
            "gone.lean",
            "import Lean.Elab.Frontend\n\ndef fromGone := 1\n",
        );
        repl.feed(&[&format!(":l {src}")]);
        std::fs::remove_file(&src).expect("remove");
        let out = repl.feed(&[":restart"]);
        assert!(out.contains("読み込み直せなかった"), "{out}");

        repl.feed(&["def afterRestart := 2"]);
        let out_path = repl.tmp().join("after.lean").display().to_string();
        repl.feed(&[&format!(":save {out_path}")]);
        let text = std::fs::read_to_string(&out_path).expect("read");
        // ファイルの中身はもう環境に無いので、その import だけを書き出す理由も無い。
        assert!(!text.contains("Lean.Elab.Frontend"), "{text}");
    }
}

mod module_files {
    //! ja: module のファイル
    //! en: Module files
    //!
    //! Lean の module system のファイル。`module` の行は import より前に書く決まりで、
    //! module から module でないファイルは import できない。
    use super::*;

    /// `public import Lean` が無いと、`elab` を書いたファイルは
    /// "imported privately" で失敗する。
    const SRC: &str = "module\n\npublic import Lean\n\n\
        open Lean Elab Command in\n\
        elab \"#hello\" : command => logInfo \"hello\"\n\n\
        public def five := 5\n";

    /// ja: :l すると宣言を読み込み、:save すると module の行で始まるファイルを書き出す
    /// en: Loads the declarations on :l and saves a file that starts with the module line on :save
    #[story("B4")]
    #[test]
    fn loads_the_declarations_on_l_and_saves_a_file_that_starts_with_the_module_line_on_save() {
        let mut repl = Driver::new();
        let src = file(&repl, "Mod.lean", SRC);
        let out = repl.feed(&[&format!(":l {src}")]);
        assert!(!out.contains("読み込めなかった"), "{out}");
        assert!(repl.feed(&["five"]).contains('5'));

        // :save したファイルも module のままで、もう一度 :l できる。
        let saved = repl.tmp().join("Saved.lean").display().to_string();
        repl.feed(&[&format!(":save {saved}")]);
        let text = std::fs::read_to_string(&saved).expect("read");
        assert!(text.starts_with("module\n"), "{text}");
        let out = repl.feed(&[&format!(":l {saved}")]);
        assert!(!out.contains("読み込めなかった"), "{out}\n{text}");
        assert!(repl.feed(&["five"]).contains('5'));
    }

    /// ja: 設定に import のある環境で :l すると、設定の import の代わりに public import Lean を足す
    /// en: Adds public import Lean instead of the configured imports when :l runs in an environment with imports
    #[story("B4")]
    #[test]
    fn adds_public_import_lean_instead_of_the_configured_imports_when_l_runs_in_an_environment_with_imports()
     {
        // 既定の core は設定の import が空なので、wide に切り替えてから確かめる。
        // wide の import (Lean.Elab.Frontend) は module なので、読み込めるかどうか
        // では確かめられない。送ったリクエストを見る。
        let mut repl = Driver::new();
        repl.feed(&[":env wide"]);
        let src = file(&repl, "Plain.lean", "module\n\npublic def m := 1\n");
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        repl.hook(move |stage| {
            if let Stage::Before(obj) = stage
                && fresh(obj)
            {
                log.lock().expect("lock").push(cmd(obj).to_string());
            }
            Hooked::Pass
        });
        repl.feed(&[&format!(":l {src}")]);
        repl.unhook();
        let sent = sent.lock().expect("lock");
        let load = sent
            .iter()
            .find(|c| c.contains("public def m"))
            .expect(":l のリクエストが無い");
        assert!(load.starts_with("module\npublic import Lean\n"), "{load}");
        assert!(!load.contains("Lean.Elab.Frontend"), "{load}");
    }
}

mod imports_written_in_comments {
    //! ja: コメントに書いた import
    //! en: Imports written in comments
    use super::*;

    /// ja: :save すると、その import を書かずに、:l で読み込めるファイルを書き出す
    /// en: Saves a loadable file without the commented import when :save runs
    #[story("B3", "B4")]
    #[test]
    fn saves_a_loadable_file_without_the_commented_import_when_save_runs() {
        let mut repl = Driver::new();
        let lib = file(
            &repl,
            "doc.lean",
            "/-\nimport Nope.NotAModule\n-/\ndef documented := 1\n",
        );
        repl.feed(&[&format!(":l {lib}")]);

        let out_path = repl.tmp().join("doc-out.lean").display().to_string();
        repl.feed(&[&format!(":save {out_path}")]);
        let text = std::fs::read_to_string(&out_path).expect("read");
        let head = text.split("\n\n").next().unwrap_or("");

        // ヘッダに書くと repl はヘッダ全体を捨てて起動するので、:l でも lean でも
        // エラーになるファイルができる。それでも :save は成功を報告する。
        assert!(head.contains("import Lean"), "{head}");
        assert!(!head.contains("NotAModule"), "{head}");
        assert!(
            !repl
                .feed(&[&format!(":l {out_path}")])
                .contains("読み込めなかった")
        );
    }
}

mod when_the_base_environment_is_lost {
    //! ja: 元になる環境を失ったとき
    //! en: When the base environment is lost
    //!
    //! boot が失敗すると環境がどこにも無い。この状態で操作を受け付けても、事実と違う報告を
    //! せず、入力したテキストを失わないことだけは守る (原因を直せば :restart で戻る)。
    use super::*;

    /// boot が失敗するエンジンにする。保留に宣言 1 件を残す。
    ///
    /// boot のリクエスト (env を付けない) にエラーの応答を返す。プロセスは動いたままなので、
    /// 差し込み口を外せば次の `:restart` で戻る。
    fn no_env(repl: &mut Driver) {
        repl.feed(&["def held := 1"]);
        repl.hook(|stage| match stage {
            Stage::Before(obj) if fresh(obj) => Hooked::Reply(json!({
                "messages": [{ "severity": "error", "data": "boot に失敗させた" }]
            })),
            _ => Hooked::Pass,
        });
        repl.feed(&[":restart"]);
        assert!(repl.repl.eng().env().is_none());
        assert_eq!(repl.repl.eng().unplayed(), ["def held := 1"]);
    }

    /// ja: :reset すると、元になる環境が無いと表示して保留を残す
    /// en: Says there is no base environment and keeps the pending declaration when :reset runs
    #[story("F1", "F2")]
    #[test]
    fn says_there_is_no_base_environment_and_keeps_the_pending_declaration_when_reset_runs() {
        let mut repl = Driver::new();
        no_env(&mut repl);

        let out = repl.feed(&[":reset"]);
        // base は終了したプロセスの id。設定し直すと submit の「env が無い」ガードが
        // 無効になり、repl はどの入力にも "Unknown environment." を返すだけになる
        // (何を書いてもエラーになる)。
        assert!(out.contains("元になる環境が無い"), "{out}");
        assert_eq!(repl.repl.eng().unplayed(), ["def held := 1"]);

        repl.unhook();
        assert!(repl.feed(&[":restart"]).contains("1 件"));
        assert!(repl.feed(&["#eval held"]).contains('1'));
    }

    /// ja: 宣言を再実行しようとすると、再実行できなかったと表示して保留にする
    /// en: Says the replay failed and keeps the declarations pending when a replay starts
    #[story("F1")]
    #[test]
    fn says_the_replay_failed_and_keeps_the_declarations_pending_when_a_replay_starts() {
        let mut repl = Driver::new();
        no_env(&mut repl);

        let out = repl.feed_with("replay_into", |r| {
            r.replay_into(&["def two := 2".to_string()])
        });

        // 実行すると repl は env 無しのリクエストから Init だけの環境を新しく
        // 作ってしまう。
        // 「再実行した」と報告しながら、設定の import が無い環境に宣言を追加する
        // ことになる。
        assert!(out.contains("再実行できなかった"), "{out}");
        assert_eq!(
            repl.repl.eng().unplayed(),
            ["def two := 2", "def held := 1"]
        );
        assert!(repl.repl.eng().log().is_empty());
    }

    /// ja: sorry を持ち越したまま環境を失うと、その sorry を捨て、:prove で証明モードを始めない
    /// en: Drops a carried-over sorry when the environment is lost, so :prove does not start proof mode
    #[story("F1", "E1")]
    #[test]
    fn drops_a_carried_over_sorry_when_the_environment_is_lost_so_prove_does_not_start_proof_mode()
    {
        let mut repl = Driver::new();
        repl.feed(&["theorem ghostSorry : True := by sorry"]);
        assert!(repl.repl.held().is_some());

        // 完結判定でエンジンが終了し、再起動の boot も失敗する。
        let mut dead = false;
        repl.hook(move |stage| match stage {
            Stage::Before(obj) if fresh(obj) => Hooked::Kill,
            Stage::Before(obj) if !dead && obj.get("cmd").is_some() => {
                dead = true;
                Hooked::Kill
            }
            _ => Hooked::Pass,
        });
        repl.feed(&["def afterGhost := 1"]);

        // 残すと :goals が環境に無い宣言のゴールを表示し、:prove がその無効な
        // proofState で証明モードを始める。
        assert!(repl.repl.held().is_none());
        assert!(repl.feed(&[":prove"]).contains("sorry")); // 「sorry が無い」と表示するだけ
        assert!(repl.repl.proof().is_none());
    }

    /// ja: 今の環境に送るコマンドには NoEnvironment を返し、新しい環境を作るコマンドはそのまま送る
    /// en: Returns NoEnvironment for a command on the environment and sends one that starts a new environment
    #[story("F1")]
    #[test]
    fn returns_noenvironment_for_a_command_on_the_environment_and_sends_one_that_starts_a_new_environment()
     {
        let mut repl = Driver::new();
        no_env(&mut repl);
        repl.unhook();

        // 環境が無いときにどうするかを決めていない呼び出しは、ここでエラーになる。
        // env キーを付けずに送ると repl は Init だけの環境を作って答えるので、
        // 成功したように見える宣言が次のリクエストで消える。
        let r = repl.repl.eng_mut().send_cmd("def sneaked := 1", false);
        assert!(matches!(r, Err(EngineFault::NoEnvironment(_))), "{r:?}");
        assert!(repl.repl.eng().log().is_empty());

        // 新しい環境を作るための呼び出し (boot と :l) はそのまま送る。
        let resp = repl
            .repl
            .eng_mut()
            .send_cmd("#check Nat", true)
            .expect("fresh");
        assert!(resp.get("env").is_some(), "{resp}");
    }

    /// ja: :t を実行すると、型の代わりに取得できなかったと表示する
    /// en: Says the type could not be obtained when :t runs
    #[story("F1", "D2")]
    #[test]
    fn says_the_type_could_not_be_obtained_when_t_runs() {
        let mut repl = Driver::new();
        no_env(&mut repl);

        // 答えると Init だけの環境からの答えになる。設定の import が無いので
        // 正しくない答えになるうえ、「エンジンは正常」に見えてしまう。
        assert!(repl.feed(&[":t 1 + 1"]).contains("取得できなかった"));
        assert!(repl.repl.eng().env().is_none());
    }
}

mod files_with_unresolvable_imports {
    //! ja: 解決できない import のファイル
    //! en: Files with unresolvable imports
    use super::*;

    /// ja: :l すると、読み込めなかったと表示して今の環境を残す
    /// en: Says the file could not be loaded and keeps the current environment when :l runs
    #[story("B4", "G4")]
    #[test]
    fn says_the_file_could_not_be_loaded_and_keeps_the_current_environment_when_l_runs() {
        let mut repl = Driver::new();
        // 本文は空にしておく。宣言があると、ヘッダを捨てられた環境でその宣言が
        // エラーになり、probe の確認より前に失敗してしまう。
        let path = file(
            &repl,
            "badimport.lean",
            "import NoSuchModuleXYZ\n\n-- 中身は無い\n",
        );
        let out = repl.feed(&[&format!(":l {path}")]);

        // import が 1 つでも解決できないと repl はヘッダを丸ごと捨てて (エラーも
        // 出さずに) 環境を返す。boot と同じ確認をしないと、「読み込んだ」と
        // 報告したうえで、完結判定も補完も宣言も失敗する環境のままになる。
        assert!(out.contains("読み込めなかった"), "{out}");
        assert!(out.contains("import をすべて無視した"), "{out}");
        assert!(
            repl.feed(&["#eval 1 + 1"]).contains('2'),
            "今の環境まで失った"
        );
    }
}

mod argument_parsing {
    //! ja: 引数の受け取り
    //! en: Argument parsing
    use super::*;

    /// ja: 数として読めない引数で :undo を実行すると、宣言を 1 件取り消す
    /// en: Undoes one declaration when :undo runs with an argument that is not a number
    #[story("F2")]
    #[test]
    fn undoes_one_declaration_when_undo_runs_with_an_argument_that_is_not_a_number() {
        // 上付きの ²。Unicode では数字の文字だが、数としては読めない。
        let mut repl = Driver::new();
        repl.feed(&["def a := 1"]);
        repl.feed(&["def b := 2"]);
        let out = repl.feed(&[":undo ²"]);
        assert_eq!(repl.declarations().len(), 1, "{out}");
    }
}

mod checks_before_sending_to_the_engine {
    //! ja: 送る前の確認
    //! en: Checks before sending to the engine
    use super::*;

    /// ja: エンジンに環境が無いときに宣言を入力すると、エンジンが使えないと表示して宣言を記録しない
    /// en: Says the engine is unusable and records nothing when a declaration is entered without an environment
    #[story("F1")]
    #[test]
    fn says_the_engine_is_unusable_and_records_nothing_when_a_declaration_is_entered_without_an_environment()
     {
        // boot に失敗したエンジン。import 無しの環境に宣言を追加すると、
        // 以後何を書いてもエラーになる。
        let mut repl = Driver::new();
        repl.repl.eng_mut().drop_env();
        let out = repl.feed(&["def x := 1"]);
        assert!(out.contains("使えない"), "{out}");
        assert!(repl.declarations().is_empty());
    }
}

mod proof_mode {
    //! ja: 証明モード
    //! en: Proof mode
    use super::*;

    /// ja: exact? を入力して証明が終わると、exact? の代わりに提案の exact を宣言に書く
    /// en: Writes the suggested exact in place of exact? into the declaration when exact? finishes the proof
    #[story("E2")]
    #[test]
    fn writes_the_suggested_exact_in_place_of_exact_into_the_declaration_when_exact_finishes_the_proof()
     {
        // `exact?` のままだと :save したファイルで毎回検索が実行され、
        // 結果も環境によって変わる。
        let mut repl = Driver::new();
        repl.feed(&["theorem t1 (n : Nat) : n + 0 = n := by sorry", ":prove"]);
        // 1 つのタクティクで証明が終わるので、そのまま証明モードを終了する。
        let out = repl.feed(&["exact?"]);
        assert!(out.contains("スクリプトには"), "{out}");
        let done = out.find("証明完了").expect(&out);
        assert!(!out[done..].contains("exact?"), "置き換えた宣言に残った");
        let decl = repl.declarations().pop().expect("宣言が無い");
        assert!(decl.contains("exact ") && !decl.contains("sorry"), "{decl}");
    }

    /// ja: 複数行の文字列で sorry を証明すると、置き換えた宣言の値は入力した文字列と同じになる
    /// en: Keeps the entered string as the declaration's value when a multi-line string proves its sorry
    #[story("E3")]
    #[test]
    fn keeps_the_entered_string_as_the_declaration_s_value_when_a_multi_line_string_proves_its_sorry()
     {
        let mut repl = Driver::new();
        repl.feed(&["def greeting : String := by sorry", ":prove"]);
        let out = repl.feed(&["exact \"Hello", "World\""]);
        assert!(out.contains("証明完了"), "{out}");
        let out = repl.feed(&["greeting.length"]);
        assert!(out.lines().any(|l| l.trim() == "11"), "{out}");
    }

    /// ja: 閉じていないタクティクを入力すると、次の行に続けて 1 つのタクティクとして実行する
    /// en: Runs an unclosed tactic with the next line as one tactic when it is entered
    #[story("E1")]
    #[test]
    fn runs_an_unclosed_tactic_with_the_next_line_as_one_tactic_when_it_is_entered() {
        let mut repl = Driver::new();
        repl.feed(&[
            "theorem pair (p q : Prop) (hp : p) (hq : q) : p ∧ q := by sorry",
            ":prove",
        ]);
        let out = repl.feed(&["exact ⟨hp,", "hq⟩"]);
        assert!(out.contains("証明完了"), "{out}");
    }

    /// ja: セミコロンで並べたタクティクを 1 行で入力すると、すべて実行して証明を終える
    /// en: Runs all tactics and finishes the proof when one line of semicolon-separated tactics is entered
    #[story("E1")]
    #[test]
    fn runs_all_tactics_and_finishes_the_proof_when_one_line_of_semicolon_separated_tactics_is_entered()
     {
        let mut repl = Driver::new();
        repl.feed(&["theorem semi : 1 = 1 ∧ 2 = 2 := by sorry", ":prove"]);
        let out = repl.feed(&["constructor; rfl; rfl"]);
        assert!(out.contains("証明完了"), "{out}");
    }

    /// ja: パースエラーになるタクティクを入力すると、入力した行の中の位置を表示する
    /// en: Shows the position in the entered line when a tactic with a parse error is entered
    #[story("E1")]
    #[test]
    fn shows_the_position_in_the_entered_line_when_a_tactic_with_a_parse_error_is_entered() {
        let mut repl = Driver::new();
        repl.feed(&["theorem semi2 : 1 = 1 ∧ 2 = 2 := by sorry", ":prove"]);
        let out = repl.feed(&["constructor; rfl;; rfl"]);
        assert!(out.contains("<input>:1:17"), "{out}");
    }

    /// ja: 余分な語や括弧のあるタクティクを入力すると、入力したとおりの位置でパースエラーを表示する
    /// en: Shows a parse error at the entered position when a tactic has an extra token or parenthesis
    #[story("E1")]
    #[test]
    fn shows_a_parse_error_at_the_entered_position_when_a_tactic_has_an_extra_token_or_parenthesis()
    {
        let mut repl = Driver::new();
        repl.feed(&["theorem paren : 1 = 1 := by sorry", ":prove"]);
        let out = repl.feed(&["rfl rfl"]);
        assert!(out.contains("<input>:1:4: expected end of input"), "{out}");
        let out = repl.feed(&["constructor )"]);
        assert!(out.contains("<input>:1:12"), "{out}");
    }

    /// ja: 証明の途中で :goals と :script を実行すると、残りのゴールとそれまでのタクティクを表示する
    /// en: Shows the remaining goals and the tactics so far when :goals and :script run mid-proof
    #[story("E1")]
    #[test]
    fn shows_the_remaining_goals_and_the_tactics_so_far_when_goals_and_script_run_mid_proof() {
        let mut repl = Driver::new();
        repl.feed(&[
            "theorem t2 : 1 = 1 ∧ 2 = 2 := by sorry",
            ":prove",
            "constructor",
        ]);
        assert!(repl.feed(&[":goals"]).contains("1 = 1"));
        assert!(repl.feed(&[":script"]).contains("constructor"));
        repl.feed(&["rfl"]);
        assert!(repl.feed(&["rfl"]).contains("証明完了"));
    }

    /// ja: sorry の無いまま証明モードの外で :done、:goals、:script を実行すると、証明モードではないと表示する
    /// en: Says it is not in proof mode when :done, :goals or :script runs outside proof mode with no sorry
    #[story("E1")]
    #[test]
    fn says_it_is_not_in_proof_mode_when_done_goals_or_script_runs_outside_proof_mode_with_no_sorry()
     {
        // 証明が完成すると leani は自動で証明モードを終了するので、その直後に
        // ユーザーが入力することがある。
        let mut repl = Driver::new();
        assert!(repl.feed(&[":done"]).contains("証明モードではない"));
        assert!(repl.feed(&[":goals"]).contains("証明モードではない"));
        assert!(repl.feed(&[":script"]).contains("証明モードではない"));
    }

    /// ja: sorry のある宣言を :undo してから :prove を実行すると、その前の宣言を残す
    /// en: Keeps the earlier declaration when :prove runs after :undo of a declaration with a sorry
    #[story("E1", "F2")]
    #[test]
    fn keeps_the_earlier_declaration_when_prove_runs_after_undo_of_a_declaration_with_a_sorry() {
        let mut repl = Driver::new();
        repl.feed(&[
            "def keepme := 42",
            "theorem t0 : 1 = 1 := by sorry",
            ":undo",
            ":prove",
        ]);
        assert!(
            repl.feed(&["keepme"]).contains("42"),
            "手前の宣言が巻き戻された"
        );
        assert_eq!(repl.declarations().len(), 1);
    }

    /// ja: タクティクのあとで :undo を実行すると、そのタクティクの前のゴールに戻してスクリプトから除く
    /// en: Restores the goal before the last tactic and drops it from the script when :undo runs
    #[story("E1")]
    #[test]
    fn restores_the_goal_before_the_last_tactic_and_drops_it_from_the_script_when_undo_runs() {
        let mut repl = Driver::new();
        repl.feed(&[
            "theorem t3 : 1 = 1 ∧ 2 = 2 := by sorry",
            ":prove",
            "constructor",
            ":undo",
        ]);
        assert!(
            repl.feed(&[":goals"]).contains('∧'),
            "constructor の前のゴールに戻っていない"
        );
        assert!(!repl.feed(&[":script"]).contains("constructor"));
        assert!(repl.feed(&[":undo"]).contains("取り消せるタクティクが無い"));
    }

    /// ja: タクティクの直後にインデントした行を入力すると、前のタクティクと合わせて 1 つのタクティクとして実行し直す
    /// en: Reruns the previous tactic joined with an indented line when that line is entered right after it
    #[story("E1", "C2")]
    #[test]
    fn reruns_the_previous_tactic_joined_with_an_indented_line_when_that_line_is_entered_right_after_it()
     {
        let mut repl = Driver::new();
        repl.feed(&[
            "theorem t7 : 1 = 1 ∧ 2 = 2 := by sorry",
            ":prove",
            "constructor",
        ]);
        // constructor の前のゴールに戻ってから、2 行を 1 つのタクティクとして送る。
        let out = repl.block(&["  <;> rfl"]);
        assert!(out.contains("証明完了"), "{out}");
        assert!(!repl.feed(&["#print axioms t7"]).contains("sorryAx"));
    }

    /// ja: :q か :quit を入力すると、証明モードを終了し、そのあとの式を評価する
    /// en: Exits proof mode and evaluates the next expression when :q or :quit is entered
    #[story("E1")]
    #[test]
    fn exits_proof_mode_and_evaluates_the_next_expression_when_q_or_quit_is_entered() {
        for quit in [":q", ":quit"] {
            let mut repl = Driver::new();
            repl.feed(&["theorem t6 : 1 = 1 := by sorry", ":prove"]);
            assert!(repl.feed(&[quit]).contains("証明モードを終了した"));
            assert!(repl.repl.proof().is_none());
            assert!(repl.feed(&["1 + 1"]).contains('2'), "REPL まで終了した");
        }
    }

    /// ja: エラーになるタクティクを入力すると、エラーを表示し、スクリプトに記録せずゴールを変えない
    /// en: Shows the error without recording the tactic or changing the goal when a tactic fails
    #[story("E1")]
    #[test]
    fn shows_the_error_without_recording_the_tactic_or_changing_the_goal_when_a_tactic_fails() {
        let mut repl = Driver::new();
        repl.feed(&["theorem bad : 1 = 1 := by sorry", ":prove"]);
        assert!(repl.feed(&["exact nosuchLemma"]).contains("nosuchLemma"));
        assert!(repl.feed(&[":script"]).contains("(空)"));
        assert!(repl.feed(&[":goals"]).contains("1 = 1"));
    }

    /// ja: エンジンがタクティクにエラーの応答を返すと、そのエラーを表示してゴールを変えない
    /// en: Shows the error and keeps the goal when the engine returns an error response to a tactic
    #[story("E1")]
    #[test]
    fn shows_the_error_and_keeps_the_goal_when_the_engine_returns_an_error_response_to_a_tactic() {
        let mut repl = Driver::new();
        repl.feed(&["theorem lost : 1 = 1 := by sorry", ":prove"]);
        let state = repl.repl.proof().expect("proof").state;
        repl.hook(|stage| match stage {
            Stage::Before(obj) if is_tactic(obj) => {
                Hooked::Reply(json!({ "message": "Unknown proof state." }))
            }
            _ => Hooked::Pass,
        });
        assert!(repl.feed(&["rfl"]).contains("Unknown proof state."));
        assert_eq!(repl.repl.proof().expect("proof").state, state);
        repl.unhook();
        assert!(repl.feed(&[":goals"]).contains("1 = 1"));
    }

    /// ja: エンジンがタクティクの応答で PANIC すると、警告して proofState とスクリプトを変えない
    /// en: Shows a PANIC warning and keeps the proofState and script when the engine panics on a tactic
    #[story("E1", "F1")]
    #[test]
    fn shows_a_panic_warning_and_keeps_the_proofstate_and_script_when_the_engine_panics_on_a_tactic()
     {
        let mut repl = Driver::new();
        repl.feed(&["theorem panicT : 1 = 1 := by sorry", ":prove"]);
        let state = repl.repl.proof().expect("proof").state;
        repl.hook(|stage| match stage {
            Stage::Read(obj, resp) if is_tactic(obj) => Hooked::Reply(with_panic(resp)),
            _ => Hooked::Pass,
        });
        let out = repl.feed(&["rfl"]);
        assert!(out.contains("PANIC した"), "{out}");
        assert!(!out.contains("証明完了"), "{out}");
        let proof = repl.repl.proof().expect("proof");
        assert_eq!(proof.state, state);
        assert!(proof.script.is_empty());
    }

    /// ja: エンジンが読み取れない提案を返すと、読み取れなかったと表示して入力したタクティクを記録する
    /// en: Records the entered tactic and says it cannot read the suggestion when the engine returns a malformed one
    #[story("E2")]
    #[test]
    fn records_the_entered_tactic_and_says_it_cannot_read_the_suggestion_when_the_engine_returns_a_malformed_one()
     {
        let mut repl = Driver::new();
        repl.feed(&["theorem cut : 1 = 1 ∧ 2 = 2 := by sorry", ":prove"]);
        repl.hook(|stage| match stage {
            Stage::Read(obj, resp) if is_tactic(obj) => {
                Hooked::Reply(with_info(resp, "Try this: exact (foo"))
            }
            _ => Hooked::Pass,
        });
        assert!(
            repl.feed(&["constructor"])
                .contains("提案を読み取れなかった")
        );
        assert!(repl.feed(&[":script"]).contains("constructor"));
    }

    /// ja: 応答に sorry の位置が無いまま証明を終えると、位置を読み取れなかったと表示して宣言を sorry のまま残す
    /// en: Keeps the declaration with its sorry and says the position is unreadable when a proof ends with no position
    #[story("E3")]
    #[test]
    fn keeps_the_declaration_with_its_sorry_and_says_the_position_is_unreadable_when_a_proof_ends_with_no_position()
     {
        // 宣言の応答から sorry の位置を消して、位置が分からない状態を作る。
        let mut repl = Driver::new();
        repl.hook(|stage| match stage {
            Stage::Read(obj, resp) if cmd(obj).starts_with("theorem lostPos") => {
                Hooked::Reply(without_pos(resp))
            }
            _ => Hooked::Pass,
        });
        repl.feed(&["theorem lostPos : 1 = 1 := by sorry", ":prove"]);
        assert!(
            repl.feed(&["rfl"])
                .contains("sorry の位置を読み取れなかった")
        );
        assert_eq!(repl.declarations(), ["theorem lostPos : 1 = 1 := by sorry"]);
    }

    /// 応答の sorries から位置を消す。
    fn without_pos(resp: &Json) -> Json {
        let mut r = resp.clone();
        if let Some(ss) = r.get_mut("sorries").and_then(Json::as_array_mut) {
            for sy in ss {
                if let Some(o) = sy.as_object_mut() {
                    o.remove("pos");
                    o.remove("endPos");
                }
            }
        }
        r
    }

    /// ja: sorry の残る宣言があるときに証明モードの外で :goals を実行すると、その sorry のゴールを表示する
    /// en: Shows the goal of the sorry when :goals runs outside proof mode with a declaration that has a sorry
    #[story("E1")]
    #[test]
    fn shows_the_goal_of_the_sorry_when_goals_runs_outside_proof_mode_with_a_declaration_that_has_a_sorry()
     {
        let mut repl = Driver::new();
        repl.feed(&["theorem t4 : 3 = 3 := by sorry"]);
        let out = repl.feed(&[":goals"]);
        assert!(out.contains("sorry 1"), "{out}");
        assert!(out.contains("3 = 3"), "{out}");
    }
}

mod sorry_in_term_position {
    //! ja: 項の位置の sorry
    //! en: Sorry in term position
    //!
    //! `:= sorry` の位置にはタクティクを書けないので、置き換えに失敗する。
    use super::*;

    /// ja: タクティクで証明を終えると、sorry のままだと表示して元の宣言を残す
    /// en: Says the sorry stays and keeps the original declaration when a tactic finishes the proof
    #[story("E3")]
    #[test]
    fn says_the_sorry_stays_and_keeps_the_original_declaration_when_a_tactic_finishes_the_proof() {
        let mut repl = Driver::new();
        repl.feed(&["def termSorry : Nat := sorry", ":prove"]);
        let out = repl.feed(&["exact 0"]);
        assert!(out.contains("sorry のまま"), "{out}");
        // sorry のままの宣言が残っているので、名前としては引き続き参照できる。
        // 値では確かめない。sorry に依存する項は、Lean が評価を拒むため。
        assert_eq!(repl.declarations(), ["def termSorry : Nat := sorry"]);
        assert!(repl.feed(&["#print termSorry"]).contains("termSorry"));
    }
}

mod when_the_whole_environment_is_replaced {
    //! ja: 環境全体が置き換わるとき
    //! en: When the whole environment is replaced
    use super::*;

    /// ja: 証明モードで :l を実行すると、証明モードを終了する
    /// en: Exits proof mode when :l runs in proof mode
    #[story("B4", "E1")]
    #[test]
    fn exits_proof_mode_when_l_runs_in_proof_mode() {
        // 前の環境の proofState を持ったままだと、送っても新しい環境と合わない。
        let mut repl = Driver::new();
        let path = file(&repl, "other.lean", "def other := 1\n");
        repl.feed(&["theorem t5 : 1 = 1 ∧ 2 = 2 := by sorry", ":prove"]);
        repl.feed(&[&format!(":l {path}")]);
        assert!(
            repl.feed(&[":goals"]).contains("証明モードではない"),
            "proofState が残った"
        );
    }
}

mod wrapped_suggestions {
    //! ja: 折り返した提案
    //! en: Wrapped suggestions
    //!
    //! simp? の結果は 100 桁前後で折り返される。2 行目以降を取り出さないと、閉じていない
    //! スクリプトを「証明完了」として表示したまま何も知らせない (項の位置の sorry は
    //! 置き換えても再実行がエラーになるので、問題に気付く場所が無い)。
    use super::*;

    const HYPS: &str = "(hypothesisNumberOne : a = 1) (hypothesisNumberTwo : b = 2) \
        (hypothesisNumberThree : c = 3) (hypothesisNumberFour : d = 4) \
        (hypothesisNumberFive : e = 5) (hypothesisNumberSix : f = 6)";
    const SIMP: &str = "simp? [hypothesisNumberOne, hypothesisNumberTwo, hypothesisNumberThree, \
        hypothesisNumberFour, hypothesisNumberFive, hypothesisNumberSix]";

    /// ja: その提案で証明を終えると、提案の 2 行目以降も含めて sorry を置き換える
    /// en: Replaces the sorry with the whole suggestion, wrapped lines included, when it finishes the proof
    #[story("E2", "E3")]
    #[test]
    fn replaces_the_sorry_with_the_whole_suggestion_wrapped_lines_included_when_it_finishes_the_proof()
     {
        // 途中で切れたスクリプトでは置き換えが失敗し、成功したはずの証明が捨てられる。
        let mut repl = Driver::new();
        repl.feed(&[&format!(
            "theorem folded (a b c d e f : Nat) {HYPS} : a + b + c + d + e + f = 21 := by sorry"
        )]);
        repl.feed(&[":prove", SIMP]);

        let decls = repl.declarations();
        let decl = decls.last().expect("宣言");
        assert!(!decl.contains("sorry"), "{decl}");
        assert!(decl.contains("hypothesisNumberSix"), "{decl}");
        assert!(balanced(decl), "{decl}");
    }

    /// ja: 項の位置の sorry を折り返した提案で証明すると、スクリプトを最後の行まで表示する
    /// en: Shows the script to its last line when a wrapped suggestion proves a sorry in term position
    #[story("E2")]
    #[test]
    fn shows_the_script_to_its_last_line_when_a_wrapped_suggestion_proves_a_sorry_in_term_position()
    {
        // 表示したスクリプトは、置き換えが成功したかどうかに関係なく画面に残る。
        // 途中で切れていると、「証明完了」と表示したまま誰も気付けない。
        let mut repl = Driver::new();
        repl.feed(&[&format!(
            "theorem twoHoles (a b c d e f : Nat) {HYPS} : (a + b + c + d + e + f = 21) ∧ True := ⟨sorry, sorry⟩"
        )]);
        repl.feed(&[":prove 1"]);
        let out = repl.feed(&[SIMP]);

        assert!(out.contains("証明完了"), "{out}");
        assert!(
            out.contains("hypothesisNumberSix"),
            "スクリプトを途中で切った: {out}"
        );
    }
}

mod when_a_declaration_has_several_sorries {
    //! ja: sorry が複数あるとき
    //! en: When a declaration has several sorries
    //!
    //! repl は sorry ごとに位置を返すので、leani はその位置で 1 個だけを置き換える。
    //! テキストの "sorry" を数えて位置を決めると、2 個以上あるときに置き換えられず、
    //! 宣言が sorry のままなのに「証明完了」と表示することになる。
    use super::*;

    /// ja: :prove 1 で選んだ sorry を証明すると、その sorry だけを置き換える
    /// en: Replaces only the chosen sorry when :prove 1 picks it and the proof ends
    #[story("E1", "E3")]
    #[test]
    fn replaces_only_the_chosen_sorry_when_prove_1_picks_it_and_the_proof_ends() {
        let mut repl = Driver::new();
        repl.feed(&[
            "theorem two : 1 = 1 ∧ 2 = 2 := ⟨by sorry, by sorry⟩",
            ":prove 1",
        ]);
        let out = repl.feed(&["rfl"]);

        assert!(out.contains("証明完了"), "{out}");
        let decls = repl.declarations();
        let decl = decls.last().expect("宣言");
        assert!(decl.contains("by rfl"), "{decl}");
        assert_eq!(
            decl.matches("sorry").count(),
            1,
            "もう 1 つの sorry まで置き換えた: {decl}"
        );
    }

    /// ja: 1 つ目を証明したあとで :prove して残りの sorry を証明すると、sorry の無い宣言になる
    /// en: Leaves no sorry in the declaration when the remaining sorry is proved after the first
    #[story("E1", "E3")]
    #[test]
    fn leaves_no_sorry_in_the_declaration_when_the_remaining_sorry_is_proved_after_the_first() {
        let mut repl = Driver::new();
        repl.feed(&["theorem both : 1 = 1 ∧ 2 = 2 := ⟨by sorry, by sorry⟩"]);
        repl.feed(&[":prove 1", "rfl", ":prove", "rfl"]);

        let decls = repl.declarations();
        let decl = decls.last().expect("宣言");
        assert!(!decl.contains("sorry"), "{decl}");
    }

    /// ja: 単独の行にある sorry を証明すると、インデントを変えずに sorry をタクティクに置き換える
    /// en: Replaces a sorry on its own line with the tactic at the same indentation when the proof ends
    #[story("E1", "E3")]
    #[test]
    fn replaces_a_sorry_on_its_own_line_with_the_tactic_at_the_same_indentation_when_the_proof_ends()
     {
        // 前後の空白まで変えると "    sorry" が " trivial" になり、インデントが
        // 浅くなって by ブロックの外に出る (unexpected identifier)。
        let mut repl = Driver::new();
        repl.block(&[
            "example : True := by",
            "  have h : True := by",
            "    sorry",
            "  exact h",
        ]);
        repl.feed(&[":prove"]);
        let out = repl.feed(&["trivial"]);

        assert!(out.contains("証明完了"), "{out}");
        assert!(
            !out.contains('^'),
            "置き換えた宣言がエラーになっている: {out}"
        );
        let decls = repl.declarations();
        assert_eq!(
            decls.last().map(String::as_str),
            Some("example : True := by\n  have h : True := by\n    trivial\n  exact h"),
            "{decls:?}"
        );
    }

    /// ja: インデントのある宣言の sorry を順に証明すると、それぞれの sorry を置き換える
    /// en: Replaces each sorry in an indented declaration when they are proved in turn
    #[story("E1", "E3")]
    #[test]
    fn replaces_each_sorry_in_an_indented_declaration_when_they_are_proved_in_turn() {
        let mut repl = Driver::new();
        repl.block(&[
            "theorem tidy : True ∧ True := by",
            "  constructor",
            "  · sorry",
            "  · sorry",
        ]);
        for _ in 0..2 {
            repl.feed(&[":prove", "trivial"]);
        }

        assert!(
            repl.feed(&["#print axioms tidy"])
                .contains("does not depend on any axioms")
        );
        let decls = repl.declarations();
        assert_eq!(
            decls.first().map(String::as_str),
            Some("theorem tidy : True ∧ True := by\n  constructor\n  · trivial\n  · trivial"),
            "{decls:?}"
        );
    }

    /// ja: 項の位置の sorry の置き換えに失敗したあとで :prove 2 を実行すると、残りの sorry のゴールを表示する
    /// en: Shows the goal of the remaining sorry when :prove 2 runs after a term-position sorry is not replaced
    #[story("E3")]
    #[test]
    fn shows_the_goal_of_the_remaining_sorry_when_prove_2_runs_after_a_term_position_sorry_is_not_replaced()
     {
        // 項の位置の sorry にはタクティクを書けない。元に戻すときに持ち越した sorry まで
        // 捨てると、残っている sorry の証明を :prove で続けられなくなる。
        let mut repl = Driver::new();
        repl.feed(&["def two : Nat × Nat := (sorry, sorry)", ":prove 1"]);
        let out = repl.feed(&["exact 0"]);

        assert!(out.contains("sorry のまま"), "{out}");
        assert_eq!(
            repl.declarations(),
            ["def two : Nat × Nat := (sorry, sorry)"]
        );
        assert!(
            repl.feed(&[":prove 2"]).contains("Nat"),
            "残りの sorry の証明を始められない"
        );
    }
}

mod when_the_engine_changes_during_a_tactic {
    //! ja: タクティクの途中でエンジンが変わったとき
    //! en: When the engine changes during a tactic
    //!
    //! proofState は前のプロセスのもの。新しいエンジンは番号を 0 から付け直す
    //! ので、そのまま送ると別の証明の状態に対する応答が返ってくる。
    use super::*;

    /// ja: タクティクを送るときにエンジンが異常終了すると、証明モードを終了し、sorry のある宣言を残す
    /// en: Exits proof mode and keeps the declaration with the sorry when the engine crashes on a tactic
    #[story("F1", "E1")]
    #[test]
    fn exits_proof_mode_and_keeps_the_declaration_with_the_sorry_when_the_engine_crashes_on_a_tactic()
     {
        let mut repl = Driver::new();
        repl.feed(&[
            "def keep := 7",
            "theorem twoGoals : 1 = 1 ∧ 2 = 2 := by sorry",
            ":prove",
        ]);

        die_once(&mut repl, is_tactic);
        let out = repl.feed(&["constructor"]);

        assert!(
            !out.contains("証明完了"),
            "再起動したエンジンの応答をそのまま使った: {out}"
        );
        assert!(out.contains("証明モードを終了した"), "{out}");
        assert!(repl.repl.proof().is_none());

        // 証明モードを終了したのに last.proof が立っていると、続きのインデントした行で
        // 実際の宣言が pop される (取り消した中身も消えるので元に戻せない)。
        repl.unhook();
        repl.feed(&["  exact rfl"]);
        assert!(
            repl.declarations()
                .contains(&"theorem twoGoals : 1 = 1 ∧ 2 = 2 := by sorry".to_string())
        );
    }

    /// ja: タクティクを Ctrl-C で中断すると再開できると表示し、そのあと :prove して証明すると sorry を置き換える
    /// en: Says the proof can resume when a tactic is interrupted, and replaces the sorry after :prove
    #[story("A4", "F1", "E1")]
    #[test]
    fn says_the_proof_can_resume_when_a_tactic_is_interrupted_and_replaces_the_sorry_after_prove() {
        let mut repl = Driver::new();
        repl.feed(&["theorem again : 1 = 1 := by sorry", ":prove"]);

        interrupt_after_write(&mut repl, is_tactic);
        let out = repl.feed(&["rfl"]);
        assert!(out.contains("再開できる"), "{out}");

        // replay で戻った宣言の sorry を取得し直していないと、宣言はあるのに
        // :prove が「sorry が無い」と表示するだけになる。
        repl.unhook();
        repl.feed(&[":prove", "rfl"]);
        let decls = repl.declarations();
        assert!(!decls.last().expect("宣言").contains("sorry"), "{decls:?}");
    }
}

mod sorry_in_a_declaration_with_an_error {
    //! ja: エラーになった宣言の sorry
    //! en: Sorry in a declaration with an error
    //!
    //! 環境に含まれていない宣言の sorry は置き換えられない。持ち越すと、置き換えに
    //! 失敗した直後に「sorry 1 個」と表示してから「sorry 2 個」と表示し直す。
    use super::*;

    /// ja: その宣言を入力すると、ゴールと :prove の案内を表示せず、:prove は sorry が無いと表示する
    /// en: Says there is no sorry on :prove and shows no goal or hint when such a declaration is entered
    #[story("E1", "E3")]
    #[test]
    fn says_there_is_no_sorry_on_prove_and_shows_no_goal_or_hint_when_such_a_declaration_is_entered()
     {
        let mut repl = Driver::new();
        let out = repl.feed(&["theorem ng : True := ⟨by sorry, nonsense⟩"]);

        assert!(!out.contains("proofState"), "{out}");
        assert!(!out.contains(":prove"), "{out}");
        assert!(repl.repl.held().is_none());
        assert!(repl.feed(&[":prove"]).contains("sorry が無い"));
    }

    /// ja: sorry の置き換えに失敗すると、置き換えに使った宣言の sorry ではなく元の sorry の数を表示する
    /// en: Shows the original sorry count, not the sorry of the replacement declaration, when a replacement fails
    #[story("E1", "E3")]
    #[test]
    fn shows_the_original_sorry_count_not_the_sorry_of_the_replacement_declaration_when_a_replacement_fails()
     {
        let mut repl = Driver::new();
        repl.feed(&["def nums : Nat × Nat := (sorry, sorry)", ":prove 1"]);
        let out = repl.feed(&["exact 0"]); // 項の位置なのでタクティクは書けない

        assert!(out.contains("sorry のまま"), "{out}");
        assert!(!out.contains("sorry 1 [proofState"), "{out}");
        assert!(out.contains("sorry 2 個"), "{out}");
        assert_eq!(
            repl.declarations(),
            ["def nums : Nat × Nat := (sorry, sorry)"]
        );
    }
}

mod axioms_a_declaration_depends_on {
    //! ja: 依存している公理
    //! en: Axioms a declaration depends on
    use super::*;

    /// ja: sorry の無い定理に #print axioms を実行すると、公理に依存しないと表示する
    /// en: Says no axioms are used when #print axioms runs on a theorem without sorry
    #[story("E4")]
    #[test]
    fn says_no_axioms_are_used_when_print_axioms_runs_on_a_theorem_without_sorry() {
        let mut repl = Driver::new();
        repl.feed(&["theorem ax1 : 1 = 1 := rfl"]);
        assert!(
            repl.feed(&["#print axioms ax1"])
                .contains("does not depend on any axioms")
        );
    }

    /// ja: sorry のある宣言に #print axioms を実行すると、sorryAx を表示する
    /// en: Shows sorryAx when #print axioms runs on a declaration with a sorry
    #[story("E4")]
    #[test]
    fn shows_sorryax_when_print_axioms_runs_on_a_declaration_with_a_sorry() {
        let mut repl = Driver::new();
        repl.feed(&["def ax2 : Nat := sorry"]);
        assert!(repl.feed(&["#print axioms ax2"]).contains("sorryAx"));
    }

    /// ja: sorry を証明で置き換えた定理に #print axioms を実行すると、公理に依存しないと表示する
    /// en: Says no axioms are used when #print axioms runs on a theorem whose sorry was proved
    #[story("E4", "E1", "E3")]
    #[test]
    fn says_no_axioms_are_used_when_print_axioms_runs_on_a_theorem_whose_sorry_was_proved() {
        // 置き換えられたかを公理の側から確認できる。あいだに環境を進める入力を
        // 挟むと持ち越した proofState を捨てるので、続けて入力する。
        let mut repl = Driver::new();
        repl.feed(&["theorem ax3 : 1 = 1 := by sorry", ":prove", "rfl"]);
        assert!(
            repl.feed(&["#print axioms ax3"])
                .contains("does not depend on any axioms")
        );
    }
}

mod when_the_engine_crashes_while_judging_input {
    //! ja: 入力の判定の途中でエンジンが異常終了したとき
    //! en: When the engine crashes while judging input
    //!
    //! 完結判定もエンジンとの通信なので、そこでエンジンが異常終了することがある。guard が
    //! エンジンを再起動すると証明モードは終了するが、その行の処理はまだ続いている。
    use super::*;

    /// ja: 証明モードでタクティクを入力すると、その行を実行しなかったと表示し、:prove で再開できると表示する
    /// en: Says the tactic line did not run and that :prove can resume when a tactic is entered in proof mode
    #[story("F1", "E1")]
    #[test]
    fn says_the_tactic_line_did_not_run_and_that_prove_can_resume_when_a_tactic_is_entered_in_proof_mode()
     {
        let mut repl = Driver::new();
        repl.feed(&["theorem probeDied : 1 = 1 := by sorry", ":prove"]);

        die_once(&mut repl, |o| o.get("cmd").is_some());
        let out = repl.feed(&["rfl"]);
        repl.unhook();

        // 証明モードが終了したあとに command として処理すると、タクティクが
        // 宣言として送られて "expected command" になる。
        assert!(!out.contains("expected command"), "{out}");
        assert!(out.contains("実行しなかった"), "{out}");
        // そのうえ submit_cmd の clear_pending が取得し直した sorry を消すので、
        // 直前に表示した「:prove で再開できる」が正しくなくなる。
        assert!(out.contains("再開できる"), "{out}");
        let decls = repl.declarations();
        assert!(decls.last().expect("宣言").contains("sorry"), "{decls:?}");
        assert!(
            repl.feed(&[":prove"]).contains('⊢'),
            "案内どおりに再開できない"
        );
    }
}

mod when_the_completeness_judgment_is_interrupted {
    //! ja: 完結の判定を中断したとき
    //! en: When the completeness judgment is interrupted
    //!
    //! Ctrl-C で判定を止めたら、ユーザーは入力を送りたくない。判定の結果が無いまま
    //! 送ると、式も宣言として送ることになる。
    use super::*;

    /// 完結の判定のリクエストで、src を含むものか。
    fn probes(obj: &Json, src: &str) -> bool {
        cmd(obj).contains("runParserCategory") && cmd(obj).contains(src)
    }

    /// ja: 宣言の判定を Ctrl-C で止めると、実行しなかったと表示して宣言を記録しない
    /// en: Says the declaration did not run and records nothing when Ctrl-C stops its judgment
    #[story("A4", "C1")]
    #[test]
    fn says_the_declaration_did_not_run_and_records_nothing_when_ctrl_c_stops_its_judgment() {
        let mut repl = Driver::new();
        let hit = interrupt_after_write(&mut repl, |o| probes(o, "stopped"));
        let out = repl.feed(&["def stopped := 1"]);
        assert!(hit.load(SeqCst), "判定を止めていない");
        repl.unhook();

        assert!(out.contains("実行しなかった"), "{out}");
        assert!(repl.declarations().is_empty(), "{:?}", repl.declarations());
    }

    /// ja: 前の宣言の続きを空行で確定するときの判定を Ctrl-C で止めると、取り消した宣言を戻す
    /// en: Restores the undone declaration when Ctrl-C stops the judgment on the empty line ending a continuation
    #[story("A4", "C6")]
    #[test]
    fn restores_the_undone_declaration_when_ctrl_c_stops_the_judgment_on_the_empty_line_ending_a_continuation()
     {
        let mut repl = Driver::new();
        repl.feed(&["def a := 1"]);

        // 同じ src の判定は、続きの行を受け取ったときにも送る。空行で確定するときの
        // 2 回目を止める。
        let hit = Arc::new(AtomicBool::new(false));
        let seen = hit.clone();
        let mut n = 0;
        repl.hook(move |stage| match stage {
            Stage::Written(obj) if probes(obj, "+ (") => {
                n += 1;
                if n == 2 {
                    seen.store(true, SeqCst);
                    Hooked::Interrupt
                } else {
                    Hooked::Pass
                }
            }
            _ => Hooked::Pass,
        });
        repl.feed(&["  + ("]); // 前の宣言の続き。宣言は取り消され、続きを待つ
        repl.feed(&[""]);
        assert!(hit.load(SeqCst), "確定の判定を止めていない");
        repl.unhook();

        assert!(
            repl.feed(&["a"]).contains('1'),
            "取り消した宣言が戻っていない"
        );
        assert_eq!(repl.declarations().len(), 1);
    }

    /// ja: 判定でエンジンが異常終了したあとの再実行を Ctrl-C で止めると、取り消した宣言を保留の後ろに並べる
    /// en: Queues the undone declaration after the pending ones when Ctrl-C stops a replay after an engine crash
    #[story("A4", "F1")]
    #[test]
    fn queues_the_undone_declaration_after_the_pending_ones_when_ctrl_c_stops_a_replay_after_an_engine_crash()
     {
        // 取り消した宣言 r3 は、保留にした r2 より後ろにあった。r3 をすぐに再実行
        // すると、ユーザーが止めた再実行を続けることになり、:restart では r3 が r2
        // より先に実行される。
        let mut repl = Driver::new();
        for one in ["def r1 := 1", "def r2 := 2", "def r3 := 3"] {
            repl.feed(&[one]);
        }

        let (mut died, mut hit) = (false, false);
        repl.hook(move |stage| match stage {
            Stage::Before(obj) if !died && probes(obj, "+ (") => {
                died = true;
                Hooked::Kill
            }
            Stage::Written(obj) if died && !hit && cmd(obj).starts_with("def r2") => {
                hit = true;
                Hooked::Interrupt
            }
            _ => Hooked::Pass,
        });
        let out = repl.feed(&["  + ("]); // r3 の続き。r3 を取り消して判定する
        repl.unhook();

        assert!(out.contains("再実行を中断した"), "{out}");
        assert_eq!(
            out.matches("入力した行は実行しなかった").count(),
            1,
            "{out}"
        );
        assert_eq!(repl.declarations(), ["def r1 := 1"]);
        assert_eq!(repl.repl.eng().unplayed(), ["def r2 := 2", "def r3 := 3"]);
    }
}

mod pending_declarations_and_environment_switching {
    //! ja: 保留と環境の切り替え
    //! en: Pending declarations and environment switching
    //!
    //! replay でエラーになった宣言は保留 (`Engine::unplayed`) に残る。環境を作り
    //! 直す操作で何も表示せずに消すと、「保留にした」と表示した直後に
    //! 消えることになる。
    use super::*;

    /// 保留を 1 件作る。読み込んだファイルから依存先を消す。
    fn strand(repl: &mut Driver, name: &str) -> String {
        let lib = file(
            repl,
            &format!("{name}.lean"),
            &format!("def {name}Dep := 1\n"),
        );
        repl.feed(&[&format!(":l {lib}")]);
        repl.feed(&[&format!("def {name}Use := {name}Dep + 1")]);
        std::fs::write(&lib, "def other := 9\n").expect("write");
        repl.feed(&[":restart"]);
        assert_eq!(
            repl.repl.eng().unplayed(),
            [format!("def {name}Use := {name}Dep + 1")]
        );
        lib
    }

    /// ja: 保留があるときに :env で切り替えると、保留を引き継ぐ
    /// en: Keeps pending declarations when :env switches the environment
    #[story("G5", "F1")]
    #[test]
    fn keeps_pending_declarations_when_env_switches_the_environment() {
        let mut repl = Driver::new();
        strand(&mut repl, "envKeep");

        repl.feed(&[":env wide"]);
        assert_eq!(
            repl.repl.eng().unplayed(),
            ["def envKeepUse := envKeepDep + 1"]
        );
    }

    /// ja: 保留があるときに :l すると、保留を捨てたと表示する
    /// en: Says pending declarations are discarded when :l runs
    #[story("B4", "F1")]
    #[test]
    fn says_pending_declarations_are_discarded_when_l_runs() {
        let mut repl = Driver::new();
        strand(&mut repl, "loadDrop");

        let other = file(&repl, "other2.lean", "def zz := 1\n");
        let out = repl.feed(&[&format!(":l {other}")]);

        assert!(out.contains("捨てた"), "{out}");
        assert!(repl.repl.eng().unplayed().is_empty());
    }
}

mod when_the_engine_crashes_while_replacing_a_sorry {
    //! ja: sorry の置き換えの途中でエンジンが異常終了したとき
    //! en: When the engine crashes while replacing a sorry
    use super::*;

    /// ja: 証明を終えると、再起動したエンジンで sorry を置き換え、sorry のままとは表示しない
    /// en: Replaces the sorry on the restarted engine without saying it stays when the proof ends
    #[story("E3", "F1")]
    #[test]
    fn replaces_the_sorry_on_the_restarted_engine_without_saying_it_stays_when_the_proof_ends() {
        let mut repl = Driver::new();
        repl.feed(&["theorem died : True := by sorry", ":prove"]);

        die_once(&mut repl, |o| cmd(o).contains("by trivial"));
        let out = repl.feed(&["trivial"]);
        repl.unhook();

        // guard がエンジンを再起動して再送し、置き換えた宣言は成功している。世代だけを
        // 見て「sorry のまま」と報告すると、sorry のままのテキストを再実行して
        // 重複エラーになった宣言が保留にずっと残る (:restart のたびに
        // 「再実行に失敗した宣言」と表示される)。
        assert_eq!(repl.declarations(), ["theorem died : True := by trivial"]);
        assert!(!out.contains("sorry のままにしておく"), "{out}");
        assert!(
            repl.repl.eng().unplayed().is_empty(),
            "{:?}",
            repl.repl.eng().unplayed()
        );
    }

    /// ja: 再起動したエンジンで置き換えた宣言がエラーになると、再起動したので sorry のままにすると表示する
    /// en: Keeps the sorry and says the engine restarted when the replaced declaration fails on the restarted engine
    #[story("E3", "F1")]
    #[test]
    fn keeps_the_sorry_and_says_the_engine_restarted_when_the_replaced_declaration_fails_on_the_restarted_engine()
     {
        let mut repl = Driver::new();
        repl.feed(&["theorem died2 : True := by sorry", ":prove"]);

        let mut hit = 0;
        repl.hook(move |stage| match stage {
            Stage::Before(obj) if cmd(obj).contains("by trivial") => {
                hit += 1;
                if hit == 1 {
                    Hooked::Kill
                } else {
                    let error = json!({
                        "severity": "error", "data": "boom", "pos": { "line": 1, "column": 0 }
                    });
                    Hooked::Reply(json!({ "env": 0, "messages": [error] }))
                }
            }
            _ => Hooked::Pass,
        });
        let out = repl.feed(&["trivial"]);
        repl.unhook();

        assert!(
            out.contains("エンジンを再起動したので、sorry のままにしておく"),
            "{out}"
        );
        assert_eq!(repl.declarations(), ["theorem died2 : True := by sorry"]);
    }
}

mod when_replacing_a_sorry_is_interrupted {
    //! ja: sorry の置き換えを中断したとき
    //! en: When replacing a sorry is interrupted
    use super::*;

    /// ja: 置き換えと再起動後の再実行を Ctrl-C で止めると、元の宣言を保留の後ろに並べる
    /// en: Puts the original declaration last in pending when Ctrl-C stops the replacement and the replay on restart
    #[story("E3", "F1")]
    #[test]
    fn puts_the_original_declaration_last_in_pending_when_ctrl_c_stops_the_replacement_and_the_replay_on_restart()
     {
        let mut repl = Driver::new();
        repl.feed(&[
            "def baseVal := 1",
            "theorem useIt : 1 = 1 := by sorry",
            ":prove",
        ]);

        // 置き換えた宣言の実行と、再起動のあとの def baseVal の再実行を Ctrl-C で止める。
        let mut hits = 0;
        repl.hook(move |stage| match stage {
            Stage::Written(obj)
                if (hits == 0 && cmd(obj).contains("by rfl"))
                    || (hits == 1 && cmd(obj) == "def baseVal := 1") =>
            {
                hits += 1;
                Hooked::Interrupt
            }
            _ => Hooked::Pass,
        });
        let out = repl.feed(&["rfl"]);
        repl.unhook();

        // 先に実行すると、useIt が baseVal より前に並ぶ。
        assert!(repl.declarations().is_empty(), "{out}");
        assert_eq!(
            repl.repl.eng().unplayed(),
            ["def baseVal := 1", "theorem useIt : 1 = 1 := by sorry"],
            "{out}"
        );
        assert_eq!(out.matches("件を再実行した").count(), 1, "{out}");
    }
}

mod proof_mode_after_a_failed_restart {
    //! ja: 再起動に失敗したときの証明モード
    //! en: Proof mode after a failed restart
    use super::*;

    /// ja: タクティクの判定中にエンジンが異常終了すると、再起動できなかったと表示して証明モードを終了する
    /// en: Says the restart failed and exits proof mode when the engine crashes while judging a tactic
    #[story("F1", "E1")]
    #[test]
    fn says_the_restart_failed_and_exits_proof_mode_when_the_engine_crashes_while_judging_a_tactic()
    {
        let mut repl = Driver::new();
        repl.feed(&["theorem ghost : True := by sorry", ":prove"]);

        // 完結判定でエンジンが終了し、再起動の boot も失敗する。
        let mut dead = false;
        repl.hook(move |stage| match stage {
            Stage::Before(obj) if fresh(obj) => Hooked::Kill,
            Stage::Before(obj) if !dead && obj.get("cmd").is_some() => {
                dead = true;
                Hooked::Kill
            }
            _ => Hooked::Pass,
        });
        let out = repl.feed(&["trivial"]);

        // 証明モードを終了しないと、以降どの行にも赤い "Unknown proof state." だけを返す
        // 使えない証明モードのままになる (終了する方法の案内も表示されない)。
        assert!(out.contains("再起動できなかった"), "{out}");
        assert!(out.contains("証明モードを終了した"), "{out}");
        assert!(repl.repl.proof().is_none());

        repl.unhook();
        assert!(repl.feed(&[":restart"]).contains("1 件"));
    }

    /// ja: タクティクを送るときにエンジンが異常終了すると、証明モードの終了を 1 回だけ表示する
    /// en: Says once that proof mode exited when the engine crashes as a tactic is sent
    #[story("E1", "F1")]
    #[test]
    fn says_once_that_proof_mode_exited_when_the_engine_crashes_as_a_tactic_is_sent() {
        let mut repl = Driver::new();
        repl.feed(&["theorem twice : True := by sorry", ":prove"]);

        // タクティクの送信でエンジンが終了し、再起動の boot も失敗する。
        repl.hook(move |stage| match stage {
            Stage::Before(obj) if fresh(obj) || is_tactic(obj) => Hooked::Kill,
            _ => Hooked::Pass,
        });
        let out = repl.feed(&["trivial"]);

        assert_eq!(out.matches("証明モードを終了した").count(), 1, "{out}");
        assert!(repl.repl.proof().is_none());
    }
}

mod switching_environments {
    //! ja: 環境の切り替え
    //! en: Switching environments
    use super::*;

    /// ja: import を足して起動した core で :env core を実行すると、設定の core に切り替える
    /// en: Switches to the configured core when :env core runs in a core started with extra imports
    #[story("G5")]
    #[test]
    fn switches_to_the_configured_core_when_env_core_runs_in_a_core_started_with_extra_imports() {
        // -e core -i Lean.Elab.Frontend で起動したとき。名前だけを比べると
        // 「すでに core」と表示して、設定にある core に戻れない。
        let sb = sandbox();
        let table = load_config(&sb.ctx.config).expect("config");
        let extra = ["Lean.Elab.Frontend".to_string()];
        let cfg = resolve(&sb.ctx, Some("core"), None, &extra, &table).expect("resolve");
        let out = Sink::default();
        let mut repl = Repl::new(
            &sb.ctx,
            cfg,
            None,
            Box::new(out.clone()),
            Box::new(FakeSearch::default()),
        )
        .expect("起動できない");
        let shown = || std::mem::take(&mut *out.0.lock().expect("sink"));
        shown();

        repl.feed(":env core");
        let first = shown();
        assert!(!first.contains("すでに"), "{first}");
        assert!(repl.eng().cfg().imports.is_empty(), "{first}");

        repl.feed(":env core");
        assert!(shown().contains("すでに core"));
        repl.close();
    }

    /// ja: 設定に無い今の環境の名前で :env を実行すると、すでにその環境だと表示する
    /// en: Says the environment is already in use when :env names the current one missing from the config
    #[story("G5")]
    #[test]
    fn says_the_environment_is_already_in_use_when_env_names_the_current_one_missing_from_the_config()
     {
        // -p から推測した環境は設定に無いので、設定から探すと見つからない。
        let sb = sandbox();
        let table = load_config(&sb.ctx.config).expect("config");
        let mut cfg = resolve(&sb.ctx, Some("core"), None, &[], &table).expect("resolve");
        cfg.name = "myproj".to_string();
        let out = Sink::default();
        let mut repl = Repl::new(
            &sb.ctx,
            cfg,
            None,
            Box::new(out.clone()),
            Box::new(FakeSearch::default()),
        )
        .expect("起動できない");
        let shown = || std::mem::take(&mut *out.0.lock().expect("sink"));
        shown();

        repl.feed(":env myproj");
        let got = shown();
        assert!(got.contains("すでに myproj"), "{got}");
        repl.close();
    }

    /// ja: :time と :l のあとで :env で切り替えると、:time の設定と読み込んだファイルの宣言を残す
    /// en: Keeps the :time setting and the declarations of the loaded file when :env switches the environment
    #[story("G1", "G5")]
    #[test]
    fn keeps_the_time_setting_and_the_declarations_of_the_loaded_file_when_env_switches_the_environment()
     {
        let mut repl = Driver::new();
        let path = repl.tmp().join("switched.lean").display().to_string();
        repl.feed(&["def switched := 11", &format!(":save {path}"), ":reset"]);
        repl.feed(&[&format!(":l {path}"), ":time", ":env wide"]);

        assert_eq!(repl.repl.eng().cfg().name, "wide");
        assert!(repl.repl.show_time(), ":time の設定が消えた");
        assert!(
            repl.feed(&["switched"]).contains("11"),
            ":l したファイルが引き継がれていない"
        );
    }

    /// ja: 設定に無い名前で :env を実行すると、設定に無いと表示し、そのあとの式を評価する
    /// en: Says the name is not in the config and evaluates the next expression when :env gets an unknown name
    #[story("G5")]
    #[test]
    fn says_the_name_is_not_in_the_config_and_evaluates_the_next_expression_when_env_gets_an_unknown_name()
     {
        let mut repl = Driver::new();
        assert!(repl.feed(&[":env nosuch"]).contains("設定に無い"));
        assert!(
            repl.feed(&["1 + 1"]).contains('2'),
            "環境を失ってセッションが終了している"
        );
    }

    /// ja: 起動できない環境に :env で切り替えると、起動できなかったと表示して元の環境に戻る
    /// en: Says startup failed and goes back to the original environment when :env picks one that cannot start
    #[story("G4", "G5")]
    #[test]
    fn says_startup_failed_and_goes_back_to_the_original_environment_when_env_picks_one_that_cannot_start()
     {
        // import が成功するかは boot するまで分からない。今のエンジンは kill 済み
        // なので、そのままエラーを返すとセッションごと消える。
        let mut repl = Driver::new();
        only_core(&mut repl);
        let out = repl.feed(&[":env wide"]);

        assert!(out.contains("起動できなかった"), "{out}");
        assert_eq!(repl.repl.eng().cfg().name, "core", "元の環境に戻っていない");
        assert!(repl.feed(&["1 + 1"]).contains('2'), "使えなくなっている");
    }

    /// ja: 引数なしで :env を実行すると、切り替え先の環境の一覧を表示する
    /// en: Shows the list of target environments when :env runs without arguments
    #[story("G5")]
    #[test]
    fn shows_the_list_of_target_environments_when_env_runs_without_arguments() {
        let mut repl = Driver::new();
        let out = repl.feed(&[":env"]);
        assert!(out.contains("core"), "{out}");
        assert!(out.contains("切り替え先: core wide"), "{out}");
    }

    /// ja: 今の環境の名前で :env を実行すると、すでにその環境だと表示してエンジンを再起動しない
    /// en: Says the environment is already in use and does not restart the engine when :env names the current one
    #[story("G5")]
    #[test]
    fn says_the_environment_is_already_in_use_and_does_not_restart_the_engine_when_env_names_the_current_one()
     {
        let mut repl = Driver::new();
        repl.feed(&["def stay := 1"]);
        let before = (repl.repl.eng().pid(), repl.repl.eng().generation());

        assert!(repl.feed(&[":env core"]).contains("すでに core"));
        let after = (repl.repl.eng().pid(), repl.repl.eng().generation());
        assert_eq!(after, before, "再起動した");
    }
}

mod when_switching_fails {
    //! ja: 切り替えに失敗したとき
    //! en: When switching fails
    use super::*;

    /// ja: 起動できない環境に :env で切り替えると、入力した宣言を元の環境に残す
    /// en: Keeps the entered declarations in the original environment when :env targets one that cannot start
    #[story("G4", "G5")]
    #[test]
    fn keeps_the_entered_declarations_in_the_original_environment_when_env_targets_one_that_cannot_start()
     {
        let mut repl = Driver::new();
        repl.feed(&["def typedHere := 42"]);
        only_core(&mut repl);
        let out = repl.feed(&[":env wide"]);

        assert!(out.contains("起動できなかった"), "{out}");
        // 元の環境に戻るときは新しい Engine を作る。:l したファイルの内容は preload で
        // 戻るが、対話で入力した宣言は再実行しないと消える。
        assert!(
            repl.feed(&["#eval typedHere"]).contains("42"),
            "入力した宣言が消えた"
        );
    }

    /// ja: エンジンを用意できない環境に :env で切り替えると、lake env の失敗を表示して今のエンジンを残す
    /// en: Keeps the current engine and shows the lake env failure when :env targets an environment it cannot set up
    #[story("G4", "G5")]
    #[test]
    fn keeps_the_current_engine_and_shows_the_lake_env_failure_when_env_targets_an_environment_it_cannot_set_up()
     {
        let mut repl = Driver::new();
        repl.feed(&["def typedHere := 42"]);
        let before = (repl.repl.eng().pid(), repl.repl.eng().generation());

        // lakefile を読めない Lake プロジェクトを project にすると、設定の確認は通り、
        // 準備の lake env が失敗する。
        let noproj = repl.tmp().join("noproj");
        std::fs::create_dir(&noproj).expect("mkdir");
        std::fs::write(noproj.join("lakefile.lean"), "this is not a lakefile\n").expect("write");
        add_env(
            &repl,
            &format!("\n[env.broken]\nproject = \"{}\"\n", noproj.display()),
        );
        let out = repl.feed(&[":env broken"]);

        assert!(out.contains("lake env"), "{out}");
        let after = (repl.repl.eng().pid(), repl.repl.eng().generation());
        assert_eq!(after, before, "エンジンを作り直した");
        assert!(repl.feed(&["typedHere"]).contains("42"));
    }

    /// ja: 切り替え先と元の環境のどちらも起動できないときに :env を実行すると、ループを抜けて理由を残す
    /// en: Leaves the loop and keeps the reason when :env runs and neither environment can start
    #[story("G4", "G5")]
    #[test]
    fn leaves_the_loop_and_keeps_the_reason_when_env_runs_and_neither_environment_can_start() {
        // 環境が無いまま続けても、どの入力もエラーになるだけ。feed は true (ループを
        // 抜ける) を返して理由を `env_lost` に残し、端末 (cli) はこの理由を表示して
        // 終了する。
        let mut repl = Driver::new();
        no_boot(&mut repl);
        repl.feed_with("env", |r| assert!(r.feed(":env wide"), "ループを抜けない"));

        let why = repl.repl.env_lost().expect("理由が無い");
        assert!(why.contains("にも戻れなくなった"), "{why}");
    }

    /// ja: どの環境も起動できないときに端末で :env を実行すると、内部エラーにせずに理由を返してループを抜ける
    /// en: Ends the terminal loop with the reason, not an internal error, when :env runs and no environment starts
    #[story("G4", "G5")]
    #[test]
    fn ends_the_terminal_loop_with_the_reason_not_an_internal_error_when_env_runs_and_no_environment_starts()
     {
        // ループが想定外のエラーとして扱うと、環境が無いままセッションが続く。
        // cli::run は Mutex<Repl> を受け取るので、Driver を使わずに Repl を作る。
        let sb = sandbox();
        let table = load_config(&sb.ctx.config).expect("config");
        let cfg = resolve(&sb.ctx, None, None, &[], &table).expect("resolve");
        let out = Sink::default();
        let search = FakeSearch::default();
        let mut repl =
            Repl::new(&sb.ctx, cfg, None, Box::new(out), Box::new(search)).expect("起動できない");
        repl.eng_mut().set_hook(Some(Box::new(|stage| match stage {
            Stage::Before(obj) if fresh(obj) => Hooked::Kill,
            _ => Hooked::Pass,
        })));
        let repl = Mutex::new(repl);

        let mut lines = Script(vec![":env wide", ""].into_iter());
        let got = leani::cli::run(&repl, &mut lines);

        let why = got.expect_err("ループを抜けない");
        assert!(why.contains("にも戻れなくなった"), "{why}");
        repl.into_inner().expect("repl").close();
    }

    /// ja: 元の環境に戻る起動を Ctrl-C で中断すると、宣言を保留に残し、:restart でその宣言を再実行する
    /// en: Keeps declarations pending until :restart when Ctrl-C interrupts the return to the original environment
    #[story("A4", "G5")]
    #[test]
    fn keeps_declarations_pending_until_restart_when_ctrl_c_interrupts_the_return_to_the_original_environment()
     {
        // 切り替え先の boot は失敗させ、元の環境に戻る boot は Ctrl-C で中断する。
        // 戻れなかったときと違って、環境を失った理由は無いのでセッションを続ける。
        let mut repl = Driver::new();
        repl.feed(&["def typedHere := 42"]);
        let mut boots = 0;
        repl.hook(move |stage| match stage {
            Stage::Before(obj) if fresh(obj) => {
                boots += 1;
                match boots {
                    1 => Hooked::Kill,
                    _ => Hooked::Interrupt,
                }
            }
            _ => Hooked::Pass,
        });
        let out = repl.feed_with("env", |r| {
            assert!(!r.feed(":env wide"), "ループを抜けた");
        });

        assert!(out.contains("中断した"), "{out}");
        assert!(repl.repl.env_lost().is_none());
        assert_eq!(repl.repl.eng().unplayed(), ["def typedHere := 42"]);

        repl.unhook();
        assert!(repl.feed(&[":restart"]).contains("1 件を再実行"));
        assert!(repl.feed(&["typedHere"]).contains("42"));
    }

    /// ja: :l のあとで元の環境に戻る起動を Ctrl-C で中断すると、:restart でそのファイルを読み込み直す
    /// en: Reloads the loaded file on :restart when Ctrl-C interrupts the return to the original environment
    #[story("A4", "G5")]
    #[test]
    fn reloads_the_loaded_file_on_restart_when_ctrl_c_interrupts_the_return_to_the_original_environment()
     {
        let mut repl = Driver::new();
        let path = file(&repl, "lib.lean", "def libA := 10\n");
        repl.feed(&[&format!(":l {path}")]);
        let mut boots = 0;
        repl.hook(move |stage| match stage {
            Stage::Before(obj) if fresh(obj) => {
                boots += 1;
                match boots {
                    1 => Hooked::Kill,
                    _ => Hooked::Interrupt,
                }
            }
            _ => Hooked::Pass,
        });
        let out = repl.feed_with("env", |r| {
            assert!(!r.feed(":env wide"), "ループを抜けた");
        });
        assert!(out.contains("中断した"), "{out}");

        repl.unhook();
        let out = repl.feed(&[":restart"]);
        assert!(repl.feed(&["libA"]).contains("10"), "{out}");
    }

    /// ja: 元の環境のエンジンを用意するのを Ctrl-C で中断すると、宣言を保留だけに残す
    /// en: Keeps declarations only pending when Ctrl-C interrupts setting up the original environment's engine
    #[story("F1", "G5")]
    #[test]
    fn keeps_declarations_only_pending_when_ctrl_c_interrupts_setting_up_the_original_environment_s_engine()
     {
        // 切り替え先の boot を失敗させるときにフラグを立てると、元の環境に戻るときの
        // lean --version (Engine::new の中) が中断する。新しい Engine はできないので、
        // 終了させた前のエンジンが宣言の記録を持ったまま残る。
        let mut repl = Driver::new();
        repl.feed(&["def typedHere := 42"]);
        let flag = repl.interrupted();
        repl.hook(move |stage| match stage {
            Stage::Before(obj) if fresh(obj) => {
                flag.store(true, SeqCst);
                Hooked::Kill
            }
            _ => Hooked::Pass,
        });
        let out = repl.feed(&[":env wide"]);

        assert!(out.contains("中断した"), "{out}");
        assert!(repl.declarations().is_empty(), "{out}");
        assert!(repl.repl.eng().env().is_none(), "{out}");
        assert_eq!(repl.repl.eng().unplayed(), ["def typedHere := 42"]);

        repl.unhook();
        let out = repl.feed(&[":restart"]);
        assert!(out.contains("1 件を再実行"), "{out}");
        assert_eq!(repl.declarations(), ["def typedHere := 42"]);
    }

    /// ja: 切り替えたあとの再実行を Ctrl-C で中断すると、再実行しなかった宣言の件数を表示して保留に残す
    /// en: Shows the count of declarations not replayed and keeps them pending when Ctrl-C interrupts the replay
    #[story("F1", "G5")]
    #[test]
    fn shows_the_count_of_declarations_not_replayed_and_keeps_them_pending_when_ctrl_c_interrupts_the_replay()
     {
        let mut repl = Driver::new();
        repl.feed(&["def carried := 1"]);

        interrupt_after_write(&mut repl, |o| cmd(o) == "def carried := 1");
        let out = repl.feed(&[":env wide"]);

        assert!(out.contains("再実行しなかった宣言: 1 件"), "{out}");
        assert_eq!(repl.repl.eng().unplayed(), ["def carried := 1"]);
    }
}

mod when_switching_succeeds {
    //! ja: 切り替えが成功したとき
    //! en: When switching succeeds
    use super::*;

    /// ja: :env で切り替えると、入力した宣言を新しい環境に追加する
    /// en: Adds the entered declarations to the new environment when :env switches
    #[story("G1", "G5")]
    #[test]
    fn adds_the_entered_declarations_to_the_new_environment_when_env_switches() {
        let mut repl = Driver::new();
        repl.feed(&["def carried := 42"]);
        let out = repl.feed(&[":env wide"]);

        // 切り替えが成功したときも、新しいエンジンには入力した宣言が無い。
        // 再実行しないと、入力した宣言が何も表示されずに消える。
        assert!(repl.feed(&["#eval carried"]).contains("42"), "{out}");
    }
}
