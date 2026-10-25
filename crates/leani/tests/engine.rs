//! `Engine` と `Repl` を直接呼ぶテスト。1 テストに数秒かかる。

#![allow(
    non_snake_case,
    uncommon_codepoints,
    confusable_idents,
    mixed_script_confusables
)]

mod common;

use common::{Driver, sandbox};
use leani::engine::Engine;
use leani::repl::Repl;
use leani_core::types::EngineFault;
use leani_read::config::EnvConfig;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
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
