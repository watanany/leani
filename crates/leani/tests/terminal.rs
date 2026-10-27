//! 端末が関わる部分だけ。pty 越しに本物の行編集をテストする。
//!
//! ここはエンジンの層よりさらに時間がかかるので、端末なしで確認できるものは置かない。

#![allow(
    non_snake_case,
    uncommon_codepoints,
    confusable_idents,
    mixed_script_confusables
)]

mod common;

use std::time::{Duration, Instant};

use common::pty::{PROMPT, Terminal, WIDE_PROMPT, piped};
use story::story;

mod 行編集 {
    use super::*;

    #[story("C3")]
    #[test]
    fn プロンプトの色を桁として数えない() {
        // 数えていると折り返す位置がずれ、履歴から呼び出した行を Backspace で
        // 消せなくなる。80 桁のうちプロンプトが 3 桁を使う。
        let mut term = Terminal::open(80, None, "");
        let (row, _) = term.cursor();
        term.type_keys(&"a".repeat(74));
        term.settle(0.2);
        assert_eq!(term.cursor().0, row, "74 桁で折り返している");
        assert_eq!(term.cursor().1, 77, "{}", term.cursor_line());
    }

    #[story("C3")]
    #[test]
    fn 履歴から呼び出した行を_Backspace_で修正できる() {
        let mut term = Terminal::start();
        term.line("1 + 100");
        term.type_keys("\x10"); // Ctrl-P
        term.settle(0.2);
        assert!(term.cursor_line().contains("1 + 100"));
        term.type_keys("\x7f\x7f\x7f1"); // 100 を消して 1 に
        term.settle(0.2);
        let line = term.cursor_line();
        assert!(line.trim_end().ends_with("1 + 1"), "{line}");
        assert!(term.line("").contains('2'));
    }

    #[story("F4")]
    #[test]
    fn Ctrl__C_で入力途中の行を捨てる() {
        let mut term = Terminal::start();
        term.type_keys("def half : Nat");
        term.settle(0.2);
        assert!(term.cursor_line().contains("def half"));
        term.type_keys("\x03");
        assert!(term.wait_prompt(PROMPT, 30).contains("^C"));
        assert!(
            term.line("1 + 1").contains('2'),
            "入力途中の内容が残っている"
        );
    }

    #[story("A4")]
    #[test]
    fn Ctrl__C_で評価中の計算を止めても宣言が残る() {
        let mut term = Terminal::start();
        term.line("def before := 5");
        // 評価が始まってから中断する。評価の前 (完結判定の途中など) に中断すると、
        // 確かめたいエンジンの中断にならない。
        let mark = term.dir.path().join("started");
        term.type_keys(&format!(
            "#eval do IO.FS.writeFile \"{}\" \"\"; IO.sleep 60000\r",
            mark.display()
        ));
        let t0 = Instant::now();
        while !mark.exists() {
            assert!(t0.elapsed() < Duration::from_secs(30), "評価が始まらない");
            term.settle(0.1);
        }
        term.type_keys("\x03");
        term.wait_prompt(PROMPT, 60);
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "中断しても止まらず、評価が最後まで実行された"
        );
        assert!(term.line("before").contains('5'));
    }

    #[story("C3", "F3")]
    #[test]
    fn 捨てた入力途中のブロックも履歴には残る() {
        // Ctrl-C で捨てるのは入力バッファであって、確定した行の記録ではない。
        // 長い宣言の途中で入力を間違えても、ユーザーに全体を入力し直させない。
        let mut term = Terminal::start();
        term.type_keys("def typo : Nat -> Nat\r");
        term.wait_prompt("|", 20);
        term.type_keys("\x03");
        term.wait_prompt(PROMPT, 30);
        term.type_keys("\x10"); // Ctrl-P
        term.settle(0.2);
        assert!(term.cursor_line().contains("def typo : Nat -> Nat"));
    }
}

mod 略記の入力 {
    use super::*;

    #[story("C5")]
    #[test]
    fn space_で記号に変換され_space_も残る() {
        // 確定に使った space を入力から消すと `a \to b` が `a →b` になり、ユーザーは
        // 記号を入力するたびに space を追加で入力することになる。
        let mut term = Terminal::start();
        term.type_keys("#check Nat \\to Nat");
        term.settle(0.2);
        let line = term.cursor_line();
        assert!(line.contains("#check Nat → Nat"), "{line}");
        assert!(term.line("").contains("Nat → Nat : Type"));
    }

    #[story("C8")]
    #[test]
    fn Tab_で記号に変換され_空白は入らない() {
        // `(· .succ)` では Lean が読めないので、記号のすぐあとに続けられるようにする。
        let mut term = Terminal::start();
        term.type_keys("#check (\\.\t.succ : Nat \\to\t)");
        term.settle(0.2);
        let line = term.cursor_line();
        assert!(line.contains("#check (·.succ : Nat →)"), "{line}");
    }

    #[story("C8", "D1")]
    #[test]
    fn 略記でなければ_Tab_は名前を補完する() {
        let mut term = Terminal::start();
        term.type_keys("#check Nat.suc\t");
        let end = Instant::now() + Duration::from_secs(30);
        while !term.cursor_line().contains("Nat.succ") {
            assert!(Instant::now() < end, "{}", term.cursor_line());
            term.settle(0.2);
        }
    }
}

mod 履歴 {
    use super::*;

    #[story("C3", "F3")]
    #[test]
    fn 複数行のブロックは_1_件にまとまる() {
        let mut term = Terminal::start();
        term.block(&["def merged : Nat -> Nat", "  | 0 => 1", "  | _ => 2"]);
        let entries = term.saved_history();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].matches('\n').count(), 2, "{:?}", entries[0]);
    }

    #[story("C3")]
    #[test]
    fn _1_回の_Ctrl__P_でブロック全体を呼び出して_1_つの入力として送る() {
        let mut term = Terminal::start();
        term.block(&["def merged : Nat -> Nat", "  | 0 => 1", "  | _ => 2"]);
        term.type_keys("\x10");
        term.settle(0.2);
        assert!(term.screen_text().contains("| _ => 2"));
        assert!(term.screen_text().contains("def merged"));
        // 行単位で送っていたら 1 行目だけがエラーになる。
        assert!(
            term.line_until("", PROMPT, 40)
                .contains("already been declared")
        );
    }

    #[story("F3")]
    #[test]
    fn ディレクトリを含まない履歴のパスでも前回の履歴を消さない() {
        // LEANI_HISTORY=history のような相対パスだと親のディレクトリが "" になる。
        // そのまま作ろうとすると失敗し、履歴の読み込みごとスキップされる。
        // 読み込めていない履歴に 1 行目を書き込むので、前回までの履歴がすべて消える。
        let mut term = Terminal::open(
            80,
            Some("bare-history"),
            "\n# 2026-01-01 00:00:00.000000\n+def old := 1\n",
        );
        term.line("1 + 1");
        assert!(
            term.saved_history().iter().any(|e| e == "def old := 1"),
            "前回の履歴が消えた"
        );
    }

    #[story("F3")]
    #[test]
    fn 異常終了したセッションの履歴も残る() {
        // 入力を確定するたびに追記しているので、どう終了しても履歴は残る。
        let mut term = Terminal::start();
        term.line("1 + 2");
        term.line("3 + 4");
        term.close(true);
        assert_eq!(term.saved_history(), ["1 + 2", "3 + 4"]);
    }
}

mod 環境の切り替え {
    use super::*;

    #[story("G5")]
    #[test]
    fn プロンプトが設定どおりに変わる() {
        let mut term = Terminal::start();
        term.type_keys(":env wide\r");
        term.wait_prompt(WIDE_PROMPT, 90);
        assert!(
            term.line_until("(3.14 : Float)", WIDE_PROMPT, 60)
                .contains("3.14")
        );
    }
}

mod 外部コマンド {
    use super::*;

    #[story("F4")]
    #[test]
    fn Ctrl__C_で外部コマンドを止めてもセッションが残る() {
        // 子プロセスは同じプロセスグループにいるので、Ctrl-C のシグナルは
        // leani にも届く。
        // そこで leani が終了すると、それまでに実行した宣言をすべて失う。
        let mut term = Terminal::start();
        term.line("def keepBang : Nat := 41");
        term.type_keys(":! sleep 30\r");
        term.settle(0.2);
        term.type_keys("\x03");
        assert!(term.wait_prompt(PROMPT, 30).contains("^C"));
        assert!(
            term.line("keepBang + 1").contains("42"),
            "セッションが終了した"
        );
    }
}

/// パイプで渡したとき。
mod 端末でない入力 {
    use super::*;

    #[story("F4")]
    #[test]
    fn UTF__8_で読めないバイトがあっても後続の行を失わない() {
        // 読めないバイトで異常終了すると、読み込み済みの入力も一緒に消えて後続の行
        // まで失われる。leani は読めないバイトを置き換えて渡し、Lean の構文エラー
        // として報告させる。
        let dir = tempfile::tempdir().expect("tempdir");
        let out = piped(
            b"def keepPipe : Nat := 41\n\xff\xfe\nkeepPipe + 1\n",
            dir.path(),
            &[],
        );
        assert!(out.contains("42"), "{out}");
    }

    #[story("B4")]
    #[test]
    fn ファイルを指定して起動すると宣言を使える() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("given.lean");
        std::fs::write(&path, "def given := 12\n").expect("write");
        let out = piped(b"given + 1\n", dir.path(), &[&path.to_string_lossy()]);
        assert!(out.contains("13"), "{out}");
    }
}
