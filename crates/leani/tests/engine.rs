//! `Engine` と `Repl` を直接呼ぶテスト。1 テストに数秒かかる。

#![allow(
    non_snake_case,
    uncommon_codepoints,
    confusable_idents,
    mixed_script_confusables
)]

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

mod 起動時の確認 {
    //! boot と、エンジンの異常終了の報告。
    use super::*;

    #[story("G4")]
    #[test]
    fn 解決できない_import_では起動を中止する() {
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

    #[story("G4")]
    #[test]
    fn エンジンが異常終了したら終了シグナルや終了コードを報告に含める() {
        let sb = sandbox();
        let cfg = EnvConfig::make(&sb.ctx, "bogus", None, &[], None, None);
        let mut eng = Engine::new(&sb.ctx, cfg, &mut sink()).expect("起動できない");
        let pid = eng.pid().expect("プロセスが無い");
        kill(Pid::from_raw(pid.cast_signed()), Signal::SIGKILL).expect("kill");
        let why = died_message(eng.send_cmd("def after_kill := 1", true));
        assert!(why.contains("SIGKILL"), "{why}");
    }

    #[story("G4", "G5")]
    #[test]
    fn 起動に失敗したエンジンのプロセスを残さない() {
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

mod 式の評価 {
    use super::*;

    #[story("A1")]
    #[test]
    fn 単独の式は評価されて値が表示される() {
        let mut repl = Driver::new();
        assert!(repl.feed(&["1 + 1"]).contains('2'));
    }

    #[story("A1", "C1")]
    #[test]
    fn 改ページを含む文字列も式として評価する() {
        // 完結の判定に送る文字列リテラルに \f と書くと、Lean が読めずに判定が
        // エラーになり、式を宣言として送ってしまう。
        let mut repl = Driver::new();
        let out = repl.feed(&["\"a\u{c}b\".length"]);
        assert!(out.lines().any(|l| l.trim() == "3"), "{out}");
    }

    #[story("A1")]
    #[test]
    fn 複数行の文字列の値を変えずに評価する() {
        // #eval に包むときに 2 行目の先頭に空白を足すと、文字列の値が変わる。
        let mut repl = Driver::new();
        let out = repl.feed(&["\"Hello", "World\".length"]);
        assert!(out.lines().any(|l| l.trim() == "11"), "{out}");
    }

    #[story("A1", "B1")]
    #[test]
    fn IO_の式はそのまま実行される() {
        let mut repl = Driver::new();
        assert!(repl.feed(&["IO.println \"hi\""]).contains("hi"));
    }

    #[story("A2")]
    #[test]
    fn 評価できない項は理由と型を表示する() {
        // Nat -> Nat に Repr が無いので #eval できない。
        let mut repl = Driver::new();
        let out = repl.feed(&["Nat.succ"]);
        assert!(out.contains("Repr"), "{out}");
        assert!(out.contains("Nat.succ (n : Nat) : Nat"), "{out}");
    }

    #[story("A2")]
    #[test]
    fn noncomputable_な定数は理由と型を表示する() {
        let mut repl = Driver::new();
        repl.feed(&["noncomputable def nc : Nat := Classical.choice ⟨1⟩"]);
        let out = repl.feed(&["nc"]);
        assert!(out.contains("noncomputable なので評価できない"), "{out}");
        assert!(out.contains("nc : Nat"), "{out}");
    }

    #[story("A1", "B1")]
    #[test]
    fn do_ブロックは_IO_として読み直す() {
        // IO.getEnv は BaseIO なので、Lean は do 全体を BaseIO と決めてしまい、
        // IO.println を書けなくなる。
        let mut repl = Driver::new();
        let out = repl.feed(&[
            "do let x ← IO.getEnv \"LEANI_NO_SUCH_VAR\"; IO.println s!\"none={x.isNone}\"",
        ]);
        assert!(out.contains("none=true"), "{out}");
        assert!(!out.to_lowercase().contains("error"), "{out}");
    }

    #[story("A1", "B1")]
    #[test]
    fn do_の外の_action_は_do_で包み直す() {
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

    #[story("A1", "B1")]
    #[test]
    fn do_で包み直した式も_IO_として読み直す() {
        let mut repl = Driver::new();
        let out =
            repl.feed(&["IO.println s!\"none={(← IO.getEnv \"LEANI_NO_SUCH_VAR\").isNone}\""]);
        assert!(out.contains("none=true"), "{out}");
        assert!(!out.to_lowercase().contains("error"), "{out}");
    }

    #[story("A5")]
    #[test]
    fn do_で包み直した式のエラー位置は元の入力の中を指す() {
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

    #[story("B1")]
    #[test]
    fn do_ブロックでファイルを読める() {
        let mut repl = Driver::new();
        let path = repl.tmp().join("note.txt");
        std::fs::write(&path, "from file\n").expect("write");
        let out = repl.feed(&[&format!(
            "do let s ← IO.FS.readFile \"{}\"; IO.print s",
            path.display()
        )]);
        assert!(out.contains("from file"), "{out}");
    }

    #[story("B1")]
    #[test]
    fn do_ブロックでプロセスを起動して出力を受け取れる() {
        let mut repl = Driver::new();
        let out = repl.feed(&[
            "do let r ← IO.Process.output {cmd := \"echo\", args := #[\"hi\"]}; IO.print r.stdout",
        ]);
        assert!(out.contains("hi"), "{out}");
        assert!(!out.to_lowercase().contains("error"), "{out}");
    }
}

mod 複数行の宣言 {
    use super::*;

    #[story("C1")]
    #[test]
    fn 空行までをまとめて_1_つの宣言として実行する() {
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

    #[story("C2")]
    #[test]
    fn 確定した直後のインデントした行は前の入力の続きとして扱う() {
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

    #[story("C2")]
    #[test]
    fn 空行で確定したブロックの直後のインデントした行も続きとして扱う() {
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

    #[story("C1")]
    #[test]
    fn エラーになった宣言は環境を進めない() {
        let mut repl = Driver::new();
        repl.feed(&["def broken : Nat := \"oops\""]);
        assert!(repl.declarations().is_empty());
    }

    #[story("C6")]
    #[test]
    fn 波括弧のあいだは空行があっても_1_つの宣言として実行する() {
        let mut repl = Driver::new();
        repl.feed(&[":{", "def gap : Nat :=", "", "  5"]);
        assert!(repl.declarations().is_empty(), ":}} の前に実行した");
        repl.feed(&[":}"]);
        assert!(repl.feed(&["gap"]).contains('5'));
        assert_eq!(repl.declarations().len(), 1);
    }

    #[story("C6")]
    #[test]
    fn 波括弧のあいだが空なら何も実行しない() {
        let mut repl = Driver::new();
        assert_eq!(repl.feed(&[":{", ":}"]), "");
        assert!(repl.declarations().is_empty());
    }
}

mod ノートブックのセルの終わり {
    use super::*;

    #[story("J1")]
    #[test]
    fn セルの終わりで入力途中の宣言を確定する() {
        let mut repl = Driver::new();
        repl.feed(&["def cellEnd : Nat :=", "  7"]);
        assert!(repl.declarations().is_empty(), "セルの終わりの前に実行した");
        repl.feed_with("end_block", Repl::end_block);
        assert_eq!(repl.declarations().len(), 1);
        assert!(repl.feed(&["cellEnd"]).contains('7'));
    }

    #[story("J1", "C6")]
    #[test]
    fn セルの終わりで閉じていない複数行ブロックは実行しない() {
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

/// パイプの EOF と端末の Ctrl-D。どちらも `Line::End` になる。
mod 入力の終わり {
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

    #[story("C6", "F4")]
    #[test]
    fn 入力の終わりで入力途中のブロックを実行する() {
        // ファイルの末尾に空行が無いと、最後のブロックは入力途中のまま EOF になる。
        // 捨てると、ファイルの最後の宣言だけが表示なしに実行されない。
        let (got, out, _) = run(vec!["#eval", "  41 + 1"]);
        assert_eq!(got, Ok(0));
        assert!(out.lines().any(|l| l.trim() == "42"), "{out}");
    }

    #[story("C6")]
    #[test]
    fn 入力の終わりで閉じていない複数行ブロックは実行せずに知らせる() {
        // `:}` を書き忘れたのか、ここで終わるつもりだったのかは区別できない。
        let (got, out, decls) = run(vec![":{", "def unclosed := 1"]);
        assert_eq!(got, Ok(0));
        assert!(out.contains(":} が無い"), "{out}");
        assert_eq!(decls, 0);
    }
}

mod エンジンが異常終了してもセッションが続く {
    use super::*;

    #[story("F1")]
    #[test]
    fn エンジンが異常終了したら再起動して宣言を_replay_する() {
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

    #[story("A4", "F1")]
    #[test]
    fn 再起動中にもう一度中断しても応答がずれない() {
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

    #[story("F1")]
    #[test]
    fn エンジンが_PANIC_したら宣言を環境に追加せず警告する() {
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

    #[story("F1")]
    #[test]
    fn 式の評価で_PANIC_したら結果を表示せず警告する() {
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

    #[story("F1")]
    #[test]
    fn 入力したものがどの経路で送られても_PANIC_を警告する() {
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

    #[story("F1")]
    #[test]
    fn 再実行してもまた異常終了したら知らせてセッションを続ける() {
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

    #[story("A4", "F1")]
    #[test]
    fn 再起動を中断しても_colon_restart_でやり直せる() {
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

    #[story("A4", "F1")]
    #[test]
    fn ファイルの読み込み直しを中断しても_colon_restart_でやり直せる() {
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

mod エンジンを再起動できないとき {
    //! boot が失敗することもある。leani は報告して続ける (異常終了しない)。
    use super::*;

    #[story("F1")]
    #[test]
    fn 再起動に失敗しても報告して異常終了しない() {
        let mut repl = Driver::new();
        repl.feed(&["def kept := 4"]);
        repl.hook(|stage| match stage {
            Stage::Before(obj) if fresh(obj) => Hooked::Kill,
            _ => Hooked::Pass,
        });
        let out = repl.feed(&[":restart"]);
        assert!(out.contains("再起動できなかった"), "{out}");
    }

    #[story("F1")]
    #[test]
    fn 失敗しても宣言を捨てない() {
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

mod replay_が途中で止まったとき {
    //! 実行できなかった宣言は保留しておくが、環境に含まれている宣言の並び (log) には
    //! 混ぜない。log は env のスタックと 1 対 1 に対応していて、混ぜると :undo と
    //! sorry の置き換えが別の宣言を消してしまう。
    use super::*;

    #[story("F1", "F2")]
    #[test]
    fn 実行できなかった宣言を環境の並びに混ぜない() {
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

    #[story("F1", "F2")]
    #[test]
    fn 次の_colon_restart_で再実行する() {
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

    #[story("A4", "F1")]
    #[test]
    fn 異常終了後の再実行を_Ctrl_C_で止めたら入力を再実行しない() {
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

mod 内部エラーでもセッションが続く {
    //! 想定外の panic は「内部エラー」として表示し、セッションを続ける。
    use super::*;

    #[story("F1")]
    #[test]
    fn panic_を捕まえて宣言を失わない() {
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

mod ファイルの読み書き {
    use super::*;

    #[story("B4")]
    #[test]
    fn 起動時に指定したファイルの関数を呼べる() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("preload.lean");
        std::fs::write(&path, "def preloaded := 11\n").expect("write");
        let mut driver = Driver::preload(&path.display().to_string());
        assert!(driver.feed(&["preloaded"]).contains("11"));
    }

    #[story("B3", "B4")]
    #[test]
    fn colon_save_したファイルを読み込み直せる() {
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

    #[story("B4", "F1")]
    #[test]
    fn 読み込みに失敗しても今の環境を失わない() {
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

    #[story("B4")]
    #[test]
    fn 読み込めないファイルは理由を表示する() {
        use std::os::unix::fs::PermissionsExt;
        let mut repl = Driver::new();
        let path = file(&repl, "locked.lean", "def x := 1\n");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        assert!(repl.feed(&[&format!(":l {path}")]).contains("読めない"));
    }

    #[story("B4")]
    #[test]
    fn 編集したファイルを_colon_r_で読み込み直す() {
        let mut repl = Driver::new();
        let path = file(&repl, "edited.lean", "def edited := 1\n");
        repl.feed(&[&format!(":l {path}")]);
        std::fs::write(&path, "def edited := 2\n").expect("write");
        repl.feed(&[":r"]);
        let out = repl.feed(&["edited"]);
        assert!(out.contains('2'), "{out}");
    }

    #[story("B4", "F2")]
    #[test]
    fn ファイルを読み込んでいなければ_colon_r_は起動直後に戻す() {
        let mut repl = Driver::new();
        repl.feed(&["def gone := 1"]);
        repl.feed(&[":r"]);
        assert!(repl.declarations().is_empty());
    }
}

mod 少しずつ組み立てる {
    //! 前に実行した宣言を次の宣言から呼べる。REPL 上で少しずつ組み立てる。
    use super::*;

    #[story("B2")]
    #[test]
    fn 実行した宣言を次の宣言から呼べる() {
        let mut repl = Driver::new();
        repl.feed(&["def step1 (n : Nat) := n * 2"]);
        repl.feed(&["def step2 (n : Nat) := step1 n + 1"]);
        assert!(repl.feed(&["#eval step2 5"]).contains("11"));
    }

    #[story("B2", "B3")]
    #[test]
    fn 組み立てた順に書き出す() {
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

mod コマンドの一覧 {
    use super::*;

    #[story("H1")]
    #[test]
    fn colon_help_でコマンドの一覧を表示する() {
        let mut repl = Driver::new();
        let out = repl.feed(&[":help"]);
        assert!(out.contains(":prove"), "{out}");
        assert!(out.contains(":env"), "{out}");
    }

    #[story("H1")]
    #[test]
    fn 証明モードの_colon_help_では証明用のコマンドを表示する() {
        let mut repl = Driver::new();
        repl.feed(&["theorem t5 : 1 = 1 := by sorry", ":prove"]);
        assert!(repl.feed(&[":help"]).contains("1 回の入力が 1 タクティク"));
    }

    #[story("H1")]
    #[test]
    fn 無いコマンドを入力したら知らせてセッションを続ける() {
        let mut repl = Driver::new();
        assert!(repl.feed(&[":nosuch"]).contains("不明なコマンド: :nosuch"));
        assert!(repl.feed(&["1 + 1"]).contains('2'));
    }

    #[story("C7")]
    #[test]
    fn colon_abbrev_で略記の一覧を表示する() {
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

mod 実行時間 {
    //! 重い計算を切り分けるための目安。デフォルトでは表示しない (ノイズになる)。
    use super::*;

    #[story("A3")]
    #[test]
    fn colon_time_で実行時間を表示する() {
        let mut repl = Driver::new();
        assert!(repl.feed(&[":time"]).contains("on"));
        let out = repl.feed(&["1 + 1"]);
        assert!(out.contains('2'), "{out}");
        assert!(out.contains("s)"), "{out}");

        assert!(repl.feed(&[":time"]).contains("off"));
        assert!(!repl.feed(&["1 + 1"]).contains("s)"));
    }
}

mod 型と_docstring {
    use super::*;

    #[story("D2")]
    #[test]
    fn colon_i_は型と_docstring_を表示する() {
        let mut repl = Driver::new();
        let out = repl.feed(&[":i Nat.succ"]);
        assert!(out.contains("Nat.succ : Nat → Nat"), "{out}");
        assert!(out.contains("successor"), "{out}");
    }

    #[story("D2")]
    #[test]
    fn 無い名前はエラーを表示する() {
        let mut repl = Driver::new();
        let out = repl.feed(&[":i Nat.nosuchThing"]);
        assert!(out.contains("不明: Nat.nosuchThing"), "{out}");
    }
}

mod 型と定義の表示 {
    use super::*;

    #[story("D2")]
    #[test]
    fn colon_t_は式の型を表示する() {
        let mut repl = Driver::new();
        for cmd in [":t", ":type"] {
            assert!(repl.feed(&[&format!("{cmd} 1 + 1")]).contains("Nat"));
        }
    }

    #[story("D3")]
    #[test]
    fn colon_p_は定義の本体を表示する() {
        let mut repl = Driver::new();
        repl.feed(&["def shown := 1 + 2"]);
        assert!(repl.feed(&[":p shown"]).contains("1 + 2"));
    }

    #[story("D3", "H1")]
    #[test]
    fn colon_p_に名前が無ければ名前が必要と伝える() {
        let mut repl = Driver::new();
        assert!(repl.feed(&[":p"]).contains(":p には名前が必要"));
    }
}

mod 定理を外部サービスで探す {
    //! :loogle は外部サービスに問い合わせるだけで、エンジンは一度も操作しない。
    use super::*;

    #[story("D4")]
    #[test]
    fn パターンが無ければ問い合わせない() {
        let mut repl = Driver::new();
        assert!(repl.feed(&[":loogle"]).contains("必要"));
        assert_eq!(repl.loogle_calls(), 0, "空の :loogle で問い合わせた");
    }

    #[story("D4")]
    #[test]
    fn 問い合わせても環境は変わらない() {
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

    #[story("D4", "F4")]
    #[test]
    fn loogle_が応答しなくてもセッションは続く() {
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

mod 書き出しの安全対策 {
    //! :save は宣言を残す操作。そこで宣言を失わせない。
    use super::*;

    #[story("B3")]
    #[test]
    fn すでにあるファイルは上書きしない() {
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

    #[story("B3")]
    #[test]
    fn リンク先の無いシンボリックリンクも上書きしない() {
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

    #[story("B3")]
    #[test]
    fn leani_が書き出したファイルは上書きする() {
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

    #[story("B3")]
    #[test]
    fn 書き込めない場所を指定しても異常終了しない() {
        let mut repl = Driver::new();
        let path = repl.tmp().join("no/such/dir/x.lean").display().to_string();
        repl.feed(&["def x := 1"]);
        let out = repl.feed(&[&format!(":save {path}")]);
        assert!(out.contains("書き出せなかった"), "{out}");
        assert!(repl.feed(&["1 + 1"]).contains('2'));
    }
}

mod 再起動のあとで読み込み直せないとき {
    use super::*;

    #[story("F1", "B4")]
    #[test]
    fn 読み込んだファイルが消えていたら理由を表示して宣言から外す() {
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

    #[story("F1", "G2")]
    #[test]
    fn init_の再実行中に異常終了しても_init_を捨てない() {
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

mod init_の扱い {
    //! init は base の環境に含めるので、環境を作り直すと消えやすい。
    use super::*;

    #[story("F1", "G2")]
    #[test]
    fn 再起動しても_init_の宣言が残る() {
        let mut repl = Driver::new();
        init_with(&mut repl, "def fromInit := 99\n");

        assert!(repl.feed(&["fromInit"]).contains("99"));
        repl.feed(&[":restart"]);
        assert!(
            repl.feed(&["fromInit"]).contains("99"),
            ":restart で init が消えた"
        );
    }

    #[story("F2", "G2")]
    #[test]
    fn colon_reset_しても_init_の宣言が残る() {
        let mut repl = Driver::new();
        init_with(&mut repl, "def fromInit := 99\n");

        repl.feed(&[":reset"]);
        assert!(
            repl.feed(&["fromInit"]).contains("99"),
            ":reset で init が消えた"
        );
    }

    #[story("G2")]
    #[test]
    fn init_に書いた_open_は入力した行でも有効になる() {
        let mut repl = Driver::new();
        init_with(&mut repl, "open Nat\n");

        assert!(repl.feed(&["succ 1"]).contains('2'));
    }

    #[story("F2", "G2")]
    #[test]
    fn colon_reset_しても_init_の_open_が残る() {
        let mut repl = Driver::new();
        init_with(&mut repl, "open Nat\n");

        repl.feed(&[":reset"]);
        assert!(repl.feed(&["succ 1"]).contains('2'));
    }

    #[story("G2", "G4")]
    #[test]
    fn エラーのある_init_は知らせて環境を変えない() {
        let mut repl = Driver::new();
        let base = repl.repl.eng().base();
        let out = init_with(&mut repl, "def bad : Nat := \"x\"\n");
        let _ = out;
        let out = repl.feed_with("apply_init", |r| r.apply_init());
        assert!(out.contains("にエラーがある"), "{out}");
        assert_eq!(repl.repl.eng().base(), base);
        assert!(repl.repl.eng().init_src().is_none());
    }

    #[story("G2", "G4")]
    #[test]
    fn 読めない_init_は理由を表示する() {
        let mut repl = Driver::new();
        let dir = repl.tmp().display().to_string(); // ディレクトリ
        repl.repl.set_init(&dir);
        let out = repl.feed_with("apply_init", |r| r.apply_init());
        assert!(out.contains("が読めない"), "{out}");
    }

    #[story("G2")]
    #[test]
    fn import_だけの_init_は何もしない() {
        let mut repl = Driver::new();
        let path = file(&repl, "init.lean", "import Lean\n");
        repl.repl.set_init(&path);
        let env = repl.repl.eng().env();
        let out = repl.feed_with("apply_init", |r| r.apply_init());
        assert_eq!(out, "");
        assert_eq!(repl.repl.eng().env(), env);
    }
}

mod 書き出しにすべての宣言を含める {
    //! log だけ書き出すと、読み込み直したファイルが元の環境と違うものになる。
    use super::*;

    #[story("B3", "G2")]
    #[test]
    fn init_と_読み込んだファイルの宣言も書き出す() {
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

mod 書き直しをやめたとき {
    //! 前の宣言の続きを書き始めた時点で、直前の宣言は取り消されている。
    use super::*;

    #[story("C2")]
    #[test]
    fn 続きを書く途中で捨てたら元の宣言が戻る() {
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

    #[story("C2")]
    #[test]
    fn 書き直しが確定したら戻さない() {
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

    #[story("C2", "E1")]
    #[test]
    fn タクティクの続きを捨てたら直前のタクティクが戻る() {
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

    #[story("C2", "E1")]
    #[test]
    fn タクティクの書き直しが確定したら戻さない() {
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

    #[story("C2", "E1")]
    #[test]
    fn sorry_を含む宣言の続きを捨てたら_prove_も戻る() {
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

mod 読み込みの中断 {
    use super::*;

    #[story("A4", "F1")]
    #[test]
    fn 中断しても環境と宣言を失わない() {
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

    #[story("A4", "F1", "G2")]
    #[test]
    fn init_の再適用中に中断したら読み込みを取り消す() {
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

mod 履歴の書き出し {
    //! 履歴は確定した入力を 1 件として、そのつど追記する。ファイル全体を書き直さないので、
    //! 読めない形式のファイルがあっても消さない。
    use super::*;
    use leani::history::BlockHistory;

    #[story("F3")]
    #[test]
    fn 読めない形式の履歴を上書きしない() {
        // readline や libedit の履歴を引き継いだ環境ではこの形式になる。読み込むときは
        // 無視されるが、書き込みは追記なので前の履歴は残る。
        let dir = tempfile::tempdir().expect("tempdir");
        let hist = dir.path().join("gnu-history");
        std::fs::write(&hist, "1 + 1\n2 + 2\n").expect("write");

        BlockHistory::new(&hist).record("3 + 3");
        let text = std::fs::read_to_string(&hist).expect("read");
        assert!(text.starts_with("1 + 1\n2 + 2\n"), "前の履歴が消えた");
    }

    #[story("C3", "F3")]
    #[test]
    fn 複数行の入力を_1_件として保存する() {
        let dir = tempfile::tempdir().expect("tempdir");
        let history = BlockHistory::new(dir.path().join("history"));
        history.record("def f : Nat -> Nat\n  | 0 => 1");
        assert_eq!(history.load(), ["def f : Nat -> Nat\n  | 0 => 1"]);
    }

    #[story("F3")]
    #[test]
    fn 標準入力が端末でなければ履歴を持たない() {
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

    #[story("C3", "F3")]
    #[test]
    fn 行ごとの追加は受け付けない() {
        // 行編集の crate は 1 行読むたびに 1 行追加しようとする。受け付けると
        // 複数行の宣言が行ごとに分かれ、呼び戻すのに Ctrl-P が何度も必要になる。
        let dir = tempfile::tempdir().expect("tempdir");
        let history = BlockHistory::new(dir.path().join("history"));
        history.append_string("  | 0 => 1");
        assert!(history.load().is_empty());
    }
}

mod 履歴から呼び出した入力 {
    //! 履歴から呼び出した複数行は、改行を含む 1 行として `Repl::feed_line` に渡される。
    use super::*;

    #[story("C3")]
    #[test]
    fn 完結したブロックは_Enter_だけで送る() {
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

    #[story("C3")]
    #[test]
    fn 途中のブロックは送らずに続きの行を待つ() {
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

mod replay_で再実行できなかったもの {
    //! 何も表示せずに消えると、ユーザーが気付く場所が無い。
    use super::*;

    #[story("F1", "B4")]
    #[test]
    fn 読み込み直せないファイルと消えた宣言を報告する() {
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

mod 書き出したファイルのヘッダ {
    use super::*;

    #[story("B3")]
    #[test]
    fn lean_でそのまま実行できる_import_を書く() {
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

mod init_と読み込みが混ざるとき {
    use super::*;

    #[story("G2", "B3")]
    #[test]
    fn 読み込んだあとも_init_を追加し直す() {
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

mod 宣言の取り消し {
    use super::*;

    #[story("F2")]
    #[test]
    fn colon_undo_n_で_n_件取り消す() {
        let mut repl = Driver::new();
        repl.feed(&["def u1 := 1"]);
        repl.feed(&["def u2 := 2"]);
        repl.feed(&["def u3 := 3"]);
        repl.feed(&[":undo 2"]);
        assert_eq!(repl.declarations(), ["def u1 := 1"]);
        assert!(repl.feed(&["u1"]).contains('1'));
    }
}

mod 保留した宣言とエンジンの世代 {
    //! 取り消した宣言の中身は env id を持つ。再起動したエンジンにその id は無い。
    use super::*;

    #[story("F1", "C2")]
    #[test]
    fn 無効になった_env_id_を設定し直さない() {
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

    #[story("C2", "E1", "F1")]
    #[test]
    fn 再起動のあとで書き直しをやめたら前の宣言の_sorry_を残さない() {
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

    #[story("F1")]
    #[test]
    fn 無効な_env_id_を持っていたらそのことを報告する() {
        let mut repl = Driver::new();
        repl.repl.eng_mut().set_env(Some(987_654)); // 無効になった世代の env id を持つ状態
        let out = repl.feed(&["def afterGhost := 1"]);

        assert!(out.to_lowercase().contains("environment"), "{out}");
        assert!(out.contains(":restart"), "{out}");
    }
}

mod 環境に無い宣言の扱い {
    //! replay でエラーになった宣言と実行できなかった宣言は env に無い。テキストは保留
    //! しておくが、そのまま書き出すとエラーになるファイルができる。
    use super::*;

    #[story("F1")]
    #[test]
    fn 再実行に失敗した宣言を保留する() {
        let mut repl = Driver::new();
        let path = file(&repl, "lib2.lean", "def libB := 10\n");
        repl.feed(&[&format!(":l {path}")]);
        repl.feed(&["def usesB := libB + 1"]);

        std::fs::write(&path, "def other := 1\n").expect("write"); // libB を消す
        let out = repl.feed(&[":restart"]);

        assert!(out.contains("再実行に失敗した宣言"), "{out}");
        assert_eq!(repl.repl.eng().unplayed(), ["def usesB := libB + 1"]);
    }

    #[story("B3", "F1")]
    #[test]
    fn colon_save_はコメントとして添える() {
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

    #[story("F2")]
    #[test]
    fn colon_reset_したら保留も捨てる() {
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

mod 読み込んだファイルの_import {
    use super::*;

    #[story("B3", "B4")]
    #[test]
    fn colon_save_に書き戻す() {
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

    #[story("B3", "F1")]
    #[test]
    fn 読み込み直せなかったファイルの_import_は_colon_save_に書かない() {
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

mod module_のファイル {
    //! Lean の module system のファイル。`module` の行は import より前に書く決まりで、
    //! module から module でないファイルは import できない。
    use super::*;

    /// `public import Lean` が無いと、`elab` を書いたファイルは
    /// "imported privately" で失敗する。
    const SRC: &str = "module\n\npublic import Lean\n\n\
        open Lean Elab Command in\n\
        elab \"#hello\" : command => logInfo \"hello\"\n\n\
        public def five := 5\n";

    #[story("B4")]
    #[test]
    fn module_のファイルを_colon_l_できる() {
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

    #[story("B4")]
    #[test]
    fn module_のファイルには設定の_import_を足さない() {
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

mod コメントに書いた_import {
    use super::*;

    #[story("B3", "B4")]
    #[test]
    fn colon_save_のヘッダに書かない() {
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

mod 元になる環境を失ったとき {
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

    #[story("F1", "F2")]
    #[test]
    fn colon_reset_は受け付けずに保留を残す() {
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

    #[story("F1")]
    #[test]
    fn 再実行せずに保留する() {
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

    #[story("F1", "E1")]
    #[test]
    fn sorry_の持ち越しも捨てる() {
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

    #[story("F1")]
    #[test]
    fn 送る前にエラーにする() {
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

    #[story("F1", "D2")]
    #[test]
    fn 型も返さない() {
        let mut repl = Driver::new();
        no_env(&mut repl);

        // 答えると Init だけの環境からの答えになる。設定の import が無いので
        // 正しくない答えになるうえ、「エンジンは正常」に見えてしまう。
        assert!(repl.feed(&[":t 1 + 1"]).contains("取得できなかった"));
        assert!(repl.repl.eng().env().is_none());
    }
}

mod 解決できない_import_のファイル {
    use super::*;

    #[story("B4", "G4")]
    #[test]
    fn 読み込んだと表示しない() {
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

mod 引数の受け取り {
    use super::*;

    #[story("F2")]
    #[test]
    fn colon_undo_の引数が数でなければ_1_件戻す() {
        // 上付きの ²。Unicode では数字の文字だが、数としては読めない。
        let mut repl = Driver::new();
        repl.feed(&["def a := 1"]);
        repl.feed(&["def b := 2"]);
        let out = repl.feed(&[":undo ²"]);
        assert_eq!(repl.declarations().len(), 1, "{out}");
    }
}

mod 送る前の確認 {
    use super::*;

    #[story("F1")]
    #[test]
    fn 起動できていないエンジンには送らない() {
        // boot に失敗したエンジン。import 無しの環境に宣言を追加すると、
        // 以後何を書いてもエラーになる。
        let mut repl = Driver::new();
        repl.repl.eng_mut().drop_env();
        let out = repl.feed(&["def x := 1"]);
        assert!(out.contains("使えない"), "{out}");
        assert!(repl.declarations().is_empty());
    }
}

mod 証明モード {
    use super::*;

    #[story("E2")]
    #[test]
    fn exact_の提案をスクリプトに追加する() {
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

    #[story("E3")]
    #[test]
    fn 複数行の文字列で証明しても_sorry_を置き換えた宣言の値は変わらない() {
        let mut repl = Driver::new();
        repl.feed(&["def greeting : String := by sorry", ":prove"]);
        let out = repl.feed(&["exact \"Hello", "World\""]);
        assert!(out.contains("証明完了"), "{out}");
        let out = repl.feed(&["greeting.length"]);
        assert!(out.lines().any(|l| l.trim() == "11"), "{out}");
    }

    #[story("E1")]
    #[test]
    fn 証明モードで閉じていないタクティクは次の行に続ける() {
        let mut repl = Driver::new();
        repl.feed(&[
            "theorem pair (p q : Prop) (hp : p) (hq : q) : p ∧ q := by sorry",
            ":prove",
        ]);
        let out = repl.feed(&["exact ⟨hp,", "hq⟩"]);
        assert!(out.contains("証明完了"), "{out}");
    }

    #[story("E1")]
    #[test]
    fn セミコロンで並べたタクティクを_1_行で実行する() {
        let mut repl = Driver::new();
        repl.feed(&["theorem semi : 1 = 1 ∧ 2 = 2 := by sorry", ":prove"]);
        let out = repl.feed(&["constructor; rfl; rfl"]);
        assert!(out.contains("証明完了"), "{out}");
    }

    #[story("E1")]
    #[test]
    fn タクティクのパースエラーは入力した行の位置で表示する() {
        let mut repl = Driver::new();
        repl.feed(&["theorem semi2 : 1 = 1 ∧ 2 = 2 := by sorry", ":prove"]);
        let out = repl.feed(&["constructor; rfl;; rfl"]);
        assert!(out.contains("<input>:1:17"), "{out}");
    }

    #[story("E1")]
    #[test]
    fn 複数のタクティクを使う証明でゴールとスクリプトを確認できる() {
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

    #[story("E1")]
    #[test]
    fn 証明モードの外で証明用のコマンドを入力すると証明モードではないと伝える() {
        // 証明が完成すると leani は自動で証明モードを終了するので、その直後に
        // ユーザーが入力することがある。
        let mut repl = Driver::new();
        assert!(repl.feed(&[":done"]).contains("証明モードではない"));
        assert!(repl.feed(&[":goals"]).contains("証明モードではない"));
        assert!(repl.feed(&[":script"]).contains("証明モードではない"));
    }

    #[story("E1", "F2")]
    #[test]
    fn colon_undo_のあとの_colon_prove_が手前の宣言を消さない() {
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

    #[story("E1")]
    #[test]
    fn colon_undo_でタクティクを_1_つ取り消す() {
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

    #[story("E1", "C2")]
    #[test]
    fn タクティクの直後にインデントした行を書くと前のタクティクの続きとして読み直す() {
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

    #[story("E1")]
    #[test]
    fn 証明モードの_colon_q_は証明モードだけを終了する() {
        for quit in [":q", ":quit"] {
            let mut repl = Driver::new();
            repl.feed(&["theorem t6 : 1 = 1 := by sorry", ":prove"]);
            assert!(repl.feed(&[quit]).contains("証明モードを終了した"));
            assert!(repl.repl.proof().is_none());
            assert!(repl.feed(&["1 + 1"]).contains('2'), "REPL まで終了した");
        }
    }

    #[story("E1")]
    #[test]
    fn エラーになったタクティクはスクリプトに記録しない() {
        let mut repl = Driver::new();
        repl.feed(&["theorem bad : 1 = 1 := by sorry", ":prove"]);
        assert!(repl.feed(&["exact nosuchLemma"]).contains("nosuchLemma"));
        assert!(repl.feed(&[":script"]).contains("(空)"));
        assert!(repl.feed(&[":goals"]).contains("1 = 1"));
    }

    #[story("E1")]
    #[test]
    fn エンジンが返したエラーを表示してゴールを変えない() {
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

    #[story("E1", "F1")]
    #[test]
    fn タクティクで_PANIC_したら警告して_proofState_を進めない() {
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

    #[story("E2")]
    #[test]
    fn 提案を読み取れなければ入力したタクティクを記録する() {
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

    #[story("E3")]
    #[test]
    fn sorry_の位置が分からなければ宣言は_sorry_のままだと知らせる() {
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

    #[story("E1")]
    #[test]
    fn 証明モードの外の_colon_goals_で残っている_sorry_を表示する() {
        let mut repl = Driver::new();
        repl.feed(&["theorem t4 : 3 = 3 := by sorry"]);
        let out = repl.feed(&[":goals"]);
        assert!(out.contains("sorry 1"), "{out}");
        assert!(out.contains("3 = 3"), "{out}");
    }
}

/// `:= sorry` の位置にはタクティクを書けないので、置き換えに失敗する。
mod 項の位置の_sorry {
    use super::*;

    #[story("E3")]
    #[test]
    fn 置き換えられなければ_sorry_のまま残す() {
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

mod 環境全体が置き換わるとき {
    use super::*;

    #[story("B4", "E1")]
    #[test]
    fn 読み込んだら証明モードを終了する() {
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

/// simp? の結果は 100 桁前後で折り返される。2 行目以降を取り出さないと、閉じていない
/// スクリプトを「証明完了」として表示したまま何も知らせない (項の位置の sorry は
/// 置き換えても再実行がエラーになるので、問題に気付く場所が無い)。
mod 折り返した提案 {
    use super::*;

    const HYPS: &str = "(hypothesisNumberOne : a = 1) (hypothesisNumberTwo : b = 2) \
        (hypothesisNumberThree : c = 3) (hypothesisNumberFour : d = 4) \
        (hypothesisNumberFive : e = 5) (hypothesisNumberSix : f = 6)";
    const SIMP: &str = "simp? [hypothesisNumberOne, hypothesisNumberTwo, hypothesisNumberThree, \
        hypothesisNumberFour, hypothesisNumberFive, hypothesisNumberSix]";

    #[story("E2", "E3")]
    #[test]
    fn 折り返した提案でも_sorry_を置き換えられる() {
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

    #[story("E2")]
    #[test]
    fn 項の位置の_sorry_でもスクリプトを切り詰めない() {
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

/// repl は sorry ごとに位置を返すので、leani はその位置で 1 個だけを置き換える。
/// テキストの "sorry" を数えて位置を決めると、2 個以上あるときに置き換えられず、
/// 宣言が sorry のままなのに「証明完了」と表示することになる。
mod sorry_が複数あるとき {
    use super::*;

    #[story("E1", "E3")]
    #[test]
    fn 選んだ_sorry_だけを置き換える() {
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

    #[story("E1", "E3")]
    #[test]
    fn 残った_sorry_を続けて証明できる() {
        let mut repl = Driver::new();
        repl.feed(&["theorem both : 1 = 1 ∧ 2 = 2 := ⟨by sorry, by sorry⟩"]);
        repl.feed(&[":prove 1", "rfl", ":prove", "rfl"]);

        let decls = repl.declarations();
        let decl = decls.last().expect("宣言");
        assert!(!decl.contains("sorry"), "{decl}");
    }

    #[story("E1", "E3")]
    #[test]
    fn 単独の行にある_sorry_のインデントを変えない() {
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

    #[story("E1", "E3")]
    #[test]
    fn インデントのある宣言でも順に置き換えられる() {
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

    #[story("E3")]
    #[test]
    fn 置き換えられなくても残りの_sorry_の証明を続けられる() {
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

/// proofState は前のプロセスのもの。新しいエンジンは番号を 0 から付け直す
/// ので、そのまま送ると別の証明の状態に対する応答が返ってくる。
mod タクティクの途中でエンジンが変わる {
    use super::*;

    #[story("F1", "E1")]
    #[test]
    fn エンジンが異常終了したら証明モードを終了して宣言を残す() {
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

    #[story("A4", "F1", "E1")]
    #[test]
    fn 中断しても証明モードを再開できる() {
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

/// 環境に含まれていない宣言の sorry は置き換えられない。持ち越すと、置き換えに
/// 失敗した直後に「sorry 1 個」と表示してから「sorry 2 個」と表示し直す。
mod エラーになった宣言の_sorry {
    use super::*;

    #[story("E1", "E3")]
    #[test]
    fn ゴールも証明モードの案内も表示しない() {
        let mut repl = Driver::new();
        let out = repl.feed(&["theorem ng : True := ⟨by sorry, nonsense⟩"]);

        assert!(!out.contains("proofState"), "{out}");
        assert!(!out.contains(":prove"), "{out}");
        assert!(repl.repl.held().is_none());
        assert!(repl.feed(&[":prove"]).contains("sorry が無い"));
    }

    #[story("E1", "E3")]
    #[test]
    fn 置き換えに失敗しても件数を報告し直さない() {
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

mod 依存している公理 {
    use super::*;

    #[story("E4")]
    #[test]
    fn print_axioms_を実行できる() {
        let mut repl = Driver::new();
        repl.feed(&["theorem ax1 : 1 = 1 := rfl"]);
        assert!(
            repl.feed(&["#print axioms ax1"])
                .contains("does not depend on any axioms")
        );
    }

    #[story("E4")]
    #[test]
    fn sorry_のままなら_sorryAx_に依存する() {
        let mut repl = Driver::new();
        repl.feed(&["def ax2 : Nat := sorry"]);
        assert!(repl.feed(&["#print axioms ax2"]).contains("sorryAx"));
    }

    #[story("E4", "E1", "E3")]
    #[test]
    fn 置き換えたら_sorryAx_が消える() {
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

/// 完結判定もエンジンとの通信なので、そこでエンジンが異常終了することがある。guard が
/// エンジンを再起動すると証明モードは終了するが、その行の処理はまだ続いている。
mod 入力の判定の途中でエンジンが異常終了する {
    use super::*;

    #[story("F1", "E1")]
    #[test]
    fn タクティクの行を宣言として送らない() {
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

mod 完結の判定を中断したとき {
    //! Ctrl-C で判定を止めたら、ユーザーは入力を送りたくない。判定の結果が無いまま
    //! 送ると、式も宣言として送ることになる。
    use super::*;

    /// 完結の判定のリクエストで、src を含むものか。
    fn probes(obj: &Json, src: &str) -> bool {
        cmd(obj).contains("runParserCategory") && cmd(obj).contains(src)
    }

    #[story("A4", "C1")]
    #[test]
    fn 判定を_Ctrl_C_で止めたら入力を送らない() {
        let mut repl = Driver::new();
        let hit = interrupt_after_write(&mut repl, |o| probes(o, "stopped"));
        let out = repl.feed(&["def stopped := 1"]);
        assert!(hit.load(SeqCst), "判定を止めていない");
        repl.unhook();

        assert!(out.contains("実行しなかった"), "{out}");
        assert!(repl.declarations().is_empty(), "{:?}", repl.declarations());
    }

    #[story("A4", "C6")]
    #[test]
    fn 途中のブロックを確定する判定を止めたら取り消した宣言を戻す() {
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

    #[story("A4", "F1")]
    #[test]
    fn 判定で再起動したときの再実行を止めたら取り消した宣言を保留の後ろに戻す() {
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

/// replay でエラーになった宣言は保留 (`Engine::unplayed`) に残る。環境を作り
/// 直す操作で何も表示せずに消すと、「保留にした」と表示した直後に
/// 消えることになる。
mod 保留と環境の切り替え {
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

    #[story("G5", "F1")]
    #[test]
    fn colon_env_の切り替えでも保留を引き継ぐ() {
        let mut repl = Driver::new();
        strand(&mut repl, "envKeep");

        repl.feed(&[":env wide"]);
        assert_eq!(
            repl.repl.eng().unplayed(),
            ["def envKeepUse := envKeepDep + 1"]
        );
    }

    #[story("B4", "F1")]
    #[test]
    fn colon_l_が保留を捨てるなら件数を表示する() {
        let mut repl = Driver::new();
        strand(&mut repl, "loadDrop");

        let other = file(&repl, "other2.lean", "def zz := 1\n");
        let out = repl.feed(&[&format!(":l {other}")]);

        assert!(out.contains("捨てた"), "{out}");
        assert!(repl.repl.eng().unplayed().is_empty());
    }
}

mod sorry_の置き換えの途中でエンジンが異常終了する {
    use super::*;

    #[story("E3", "F1")]
    #[test]
    fn 置き換えが成功したのに_sorry_のままと報告しない() {
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

    #[story("E3", "F1")]
    #[test]
    fn 再起動したあとの再実行も失敗したら_sorry_のままと知らせる() {
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

mod sorry_の置き換えを中断したとき {
    use super::*;

    #[story("E3", "F1")]
    #[test]
    fn 再起動のあとの再実行も中断したら置き換えを戻す宣言を保留の後ろに並べる() {
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

mod 再起動に失敗したときの証明モード {
    use super::*;

    #[story("F1", "E1")]
    #[test]
    fn 無効になった_proofState_を使い続けない() {
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

    #[story("E1", "F1")]
    #[test]
    fn 再起動に失敗したら証明モードの終了を_1_回だけ知らせる() {
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

mod 環境の切り替え {
    use super::*;

    #[story("G5")]
    #[test]
    fn 名前が同じでも設定の違う環境には_colon_env_で切り替える() {
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

    #[story("G5")]
    #[test]
    fn 設定に無い環境でも今の名前なら_colon_env_はすでにと表示する() {
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

    #[story("G1", "G5")]
    #[test]
    fn 切り替えても設定と読み込んだファイルが残る() {
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

    #[story("G5")]
    #[test]
    fn 設定に無い名前を指定してもセッションが続く() {
        let mut repl = Driver::new();
        assert!(repl.feed(&[":env nosuch"]).contains("設定に無い"));
        assert!(
            repl.feed(&["1 + 1"]).contains('2'),
            "環境を失ってセッションが終了している"
        );
    }

    #[story("G4", "G5")]
    #[test]
    fn 切り替え先が起動できなければ元の環境に戻る() {
        // import が成功するかは boot するまで分からない。今のエンジンは kill 済み
        // なので、そのままエラーを返すとセッションごと消える。
        let mut repl = Driver::new();
        only_core(&mut repl);
        let out = repl.feed(&[":env wide"]);

        assert!(out.contains("起動できなかった"), "{out}");
        assert_eq!(repl.repl.eng().cfg().name, "core", "元の環境に戻っていない");
        assert!(repl.feed(&["1 + 1"]).contains('2'), "使えなくなっている");
    }

    #[story("G5")]
    #[test]
    fn 引数の無い_colon_env_は今の環境と切り替え先を表示する() {
        let mut repl = Driver::new();
        let out = repl.feed(&[":env"]);
        assert!(out.contains("core"), "{out}");
        assert!(out.contains("切り替え先: core wide"), "{out}");
    }

    #[story("G5")]
    #[test]
    fn 今の環境の名前を指定したら再起動しない() {
        let mut repl = Driver::new();
        repl.feed(&["def stay := 1"]);
        let before = (repl.repl.eng().pid(), repl.repl.eng().generation());

        assert!(repl.feed(&[":env core"]).contains("すでに core"));
        let after = (repl.repl.eng().pid(), repl.repl.eng().generation());
        assert_eq!(after, before, "再起動した");
    }
}

mod 切り替えに失敗したとき {
    use super::*;

    #[story("G4", "G5")]
    #[test]
    fn 入力した宣言も元の環境に残る() {
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

    #[story("G4", "G5")]
    #[test]
    fn エンジンを用意できない環境に切り替えても今のエンジンを残す() {
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

    #[story("G4", "G5")]
    #[test]
    fn colon_env_で元の環境にも戻れなければ入力のループを抜けて理由を残す() {
        // 環境が無いまま続けても、どの入力もエラーになるだけ。feed は true (ループを
        // 抜ける) を返して理由を `env_lost` に残し、端末 (cli) はこの理由を表示して
        // 終了する。
        let mut repl = Driver::new();
        no_boot(&mut repl);
        repl.feed_with("env", |r| assert!(r.feed(":env wide"), "ループを抜けない"));

        let why = repl.repl.env_lost().expect("理由が無い");
        assert!(why.contains("にも戻れなくなった"), "{why}");
    }

    #[story("G4", "G5")]
    #[test]
    fn 端末では元の環境にも戻れなければ内部エラーにせずループを抜ける() {
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

    #[story("A4", "G5")]
    #[test]
    fn 元の環境に戻るのを中断しても宣言を失わず_colon_restart_でやり直せる() {
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

    #[story("A4", "G5")]
    #[test]
    fn 元の環境に戻るのを中断しても_colon_l_したファイルを_colon_restart_で読み込み直す() {
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

    #[story("F1", "G5")]
    #[test]
    fn 元の環境のエンジンを用意するのを中断したら宣言を保留だけに残す() {
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

    #[story("F1", "G5")]
    #[test]
    fn 切り替えたあとの再実行が中断されても宣言を保留に残す() {
        let mut repl = Driver::new();
        repl.feed(&["def carried := 1"]);

        interrupt_after_write(&mut repl, |o| cmd(o) == "def carried := 1");
        let out = repl.feed(&[":env wide"]);

        assert!(out.contains("再実行しなかった宣言: 1 件"), "{out}");
        assert_eq!(repl.repl.eng().unplayed(), ["def carried := 1"]);
    }
}

mod 切り替えが成功したとき {
    use super::*;

    #[story("G1", "G5")]
    #[test]
    fn 入力した宣言も新しい環境に追加される() {
        let mut repl = Driver::new();
        repl.feed(&["def carried := 42"]);
        let out = repl.feed(&[":env wide"]);

        // 切り替えが成功したときも、新しいエンジンには入力した宣言が無い。
        // 再実行しないと、入力した宣言が何も表示されずに消える。
        assert!(repl.feed(&["#eval carried"]).contains("42"), "{out}");
    }
}
