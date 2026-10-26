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

/// テストの一時ディレクトリにファイルを置いて、そのパスを返す (Python 版の `tmp_path`)。
fn file(repl: &Driver, name: &str, text: &str) -> String {
    let path = repl.tmp().join(name);
    std::fs::write(&path, text).expect("write");
    path.display().to_string()
}

/// init ファイルを置いて実行する (Python 版の `INIT` の monkeypatch と `apply_init`)。
fn init_with(repl: &mut Driver, text: &str) -> String {
    let path = file(repl, "init.lean", text);
    repl.repl.set_init(&path);
    repl.feed_with("apply_init", |r| r.apply_init());
    path
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

    #[story("H1", "F4")]
    #[test]
    fn 無いコマンドを入力したら知らせてセッションを続ける() {
        let mut repl = Driver::new();
        assert!(repl.feed(&[":nosuch"]).contains("不明なコマンド: :nosuch"));
        assert!(repl.feed(&["1 + 1"]).contains('2'));
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

        // Python 版は init の送信で EngineDied を起こしていた。Rust 版の Kill は
        // プロセスを止めるので、止めずに失敗させられる Interrupt を使う。
        repl.hook(move |stage| match stage {
            Stage::Before(obj) if cmd(obj) == init_src => Hooked::Interrupt,
            _ => Hooked::Pass,
        });
        let out = repl.feed(&[":restart"]);
        assert!(out.contains("init ファイルの再実行中に"), "{out}");
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
        // Python 版は submit が呼ばれなかったことを spy で確かめていた。送っていなければ
        // 入力はバッファに残っている。
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

        std::fs::write(&path, "def libA := (10 : Nat) +\n").expect("write"); // 直せない形に壊す
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

    #[story("F1", "F2")]
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

    #[story("B3", "F2")]
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
    /// Python 版は `Engine.boot` を差し替えて例外にしていた。Rust 版は boot の
    /// リクエスト (env を付けない) にエラーの応答を返す。プロセスは動いたままなので、
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

    #[story("F2", "F4")]
    #[test]
    fn colon_undo_の引数が数でなくても異常終了しない() {
        // 上付きの ²。Python の isdigit() は True を返すが、int() は変換できない。
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
