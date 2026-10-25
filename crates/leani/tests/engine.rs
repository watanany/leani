//! `Engine` と `Repl` を直接呼ぶテスト。1 テストに数秒かかる。

#![allow(
    non_snake_case,
    uncommon_codepoints,
    confusable_idents,
    mixed_script_confusables
)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

use common::{Driver, PANIC, cmd, fresh, interrupt_after_write, sandbox, with_panic};
use leani::engine::{Engine, Hooked, Stage};
use leani::repl::Repl;
use leani_core::types::EngineFault;
use leani_read::config::EnvConfig;
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
}

mod 式の評価 {
    use super::*;

    #[story("A1")]
    #[test]
    fn 単独の式は評価されて値が表示される() {
        let mut repl = Driver::new();
        assert!(repl.feed(&["1 + 1"]).contains('2'));
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
}

mod 内部エラーでもセッションが続く {
    //! Python 版は想定外の例外を「内部エラー」として表示し、セッションを続けていた。
    //! Rust 版では panic がそれに当たる。
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
