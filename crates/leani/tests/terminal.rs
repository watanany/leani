//! 端末が関わる部分だけ。pty 越しに本物の行編集をテストする。
//!
//! ここはエンジンの層よりさらに時間がかかるので、端末なしで確認できるものは置かない。

mod common;

use std::time::{Duration, Instant};

use common::pty::{PROMPT, Terminal, WIDE_PROMPT, piped, shows};
use story::story;

mod line_editing {
    //! ja: 行編集
    //! en: Line editing
    use super::*;

    /// ja: 80 桁の端末に 74 文字を入力すると、プロンプトの色を数えずに、折り返さずにカーソルを 78 桁目に置く
    /// en: Puts the cursor at column 78 without wrapping or counting prompt colors when 74 characters are typed in 80 columns
    #[story("C3")]
    #[test]
    fn puts_the_cursor_at_column_78_without_wrapping_or_counting_prompt_colors_when_74_characters_are_typed_in_80_columns()
     {
        // 数えていると折り返す位置がずれ、履歴から呼び出した行を Backspace で
        // 消せなくなる。80 桁のうちプロンプトが 3 桁を使う。
        let mut term = Terminal::open(80, None, "");
        let (row, _) = term.cursor();
        term.type_keys(&"a".repeat(74));
        term.settle(0.2);
        assert_eq!(term.cursor().0, row, "74 桁で折り返している");
        assert_eq!(term.cursor().1, 77, "{}", term.cursor_line());
    }

    /// ja: Ctrl-P で呼び出した行を Backspace で直して送ると、直した行の値を表示する
    /// en: Shows the value of a line recalled with Ctrl-P when it is edited with Backspace and sent
    #[story("C3")]
    #[test]
    fn shows_the_value_of_a_line_recalled_with_ctrl_p_when_it_is_edited_with_backspace_and_sent() {
        let mut term = Terminal::start();
        term.line("1 + 100");
        term.type_keys("\x10"); // Ctrl-P
        term.settle(0.2);
        assert!(term.cursor_line().contains("1 + 100"));
        term.type_keys("\x7f\x7f\x7f1"); // 100 を消して 1 に
        term.settle(0.2);
        let line = term.cursor_line();
        assert!(line.trim_end().ends_with("1 + 1"), "{line}");
        let out = term.line("");
        assert!(shows(&out, "2"), "{out}");
    }

    /// ja: 入力途中の行で Ctrl-C を押すと、^C を表示してその行を捨てる
    /// en: Shows ^C and discards the unfinished line when Ctrl-C is pressed on it
    #[story("F4")]
    #[test]
    fn shows_c_and_discards_the_unfinished_line_when_ctrl_c_is_pressed_on_it() {
        let mut term = Terminal::start();
        term.type_keys("def half : Nat");
        term.settle(0.2);
        assert!(term.cursor_line().contains("def half"));
        term.type_keys("\x03");
        assert!(term.wait_prompt(PROMPT, 30).contains("^C"));
        let out = term.line("1 + 1");
        assert!(shows(&out, "2"), "入力途中の内容が残っている: {out}");
    }

    /// ja: 入力途中のブロックで Ctrl-D を押すと、ブロックを実行し、2 回目で終了する
    /// en: Runs the unfinished block when Ctrl-D is pressed, and exits on a second Ctrl-D
    #[story("C6")]
    #[test]
    fn runs_the_unfinished_block_when_ctrl_d_is_pressed_and_exits_on_a_second_ctrl_d() {
        // reedline は空の行でだけ Ctrl-D を返すので、` | ` の空の行で押す。
        // ブロックを実行したあとは入力途中ではないので、もう一度押すと終了する。
        let mut term = Terminal::start();
        for one in ["#eval", "  41 + 1"] {
            term.type_keys(&format!("{one}\r"));
            term.wait_prompt("|", 20);
        }
        term.type_keys("\x04");
        let out = term.wait_prompt(PROMPT, 40);
        assert!(shows(&out, "42"), "{out}");
        term.type_keys("\x04");
        assert!(term.wait_exit(10), "もう一度 Ctrl-D を押しても終了しない");
    }

    /// ja: 行編集の無い端末で Ctrl-D を押すと、入力途中のブロックを実行して次の入力を読む
    /// en: Runs the unfinished block and reads more input when Ctrl-D is pressed without line editing
    #[story("C6", "F4")]
    #[test]
    fn runs_the_unfinished_block_and_reads_more_input_when_ctrl_d_is_pressed_without_line_editing()
    {
        // 端末の Ctrl-D は 1 回だけ read を 0 で返す。パイプの終わりとは違い、
        // そのあとも入力を読める。
        let mut term = Terminal::with(&[], false);
        for one in ["#eval", "  41 + 1"] {
            term.type_keys(&format!("{one}\r"));
            term.wait_prompt("|", 20);
        }
        term.type_keys("\x04");
        let out = term.wait_prompt(PROMPT, 40);
        assert!(shows(&out, "42"), "{out}");
        let out = term.line("1 + 1");
        assert!(shows(&out, "2"), "Ctrl-D のあとに入力を読まない: {out}");
    }

    /// ja: 評価中に Ctrl-C を押すと、評価を中断し、それまでの宣言を残す
    /// en: Interrupts the evaluation and keeps earlier declarations when Ctrl-C is pressed during it
    #[story("A4")]
    #[test]
    fn interrupts_the_evaluation_and_keeps_earlier_declarations_when_ctrl_c_is_pressed_during_it() {
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
        let out = term.line("before");
        assert!(shows(&out, "5"), "{out}");
    }

    /// ja: Ctrl-C で入力途中のブロックを捨てたあと Ctrl-P を押すと、捨てた行を呼び出す
    /// en: Recalls the line of a block discarded with Ctrl-C when Ctrl-P is pressed
    #[story("C3", "F3")]
    #[test]
    fn recalls_the_line_of_a_block_discarded_with_ctrl_c_when_ctrl_p_is_pressed() {
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

mod abbreviation_input {
    //! ja: 略記の入力
    //! en: Abbreviation input
    use super::*;

    /// ja: 略記のあとに space を入力すると、記号に変換して space を残す
    /// en: Converts the abbreviation to a symbol and keeps the space when a space is typed after it
    #[story("C5")]
    #[test]
    fn converts_the_abbreviation_to_a_symbol_and_keeps_the_space_when_a_space_is_typed_after_it() {
        // 確定に使った space を入力から消すと `a \to b` が `a →b` になり、ユーザーは
        // 記号を入力するたびに space を追加で入力することになる。
        let mut term = Terminal::start();
        term.type_keys("#check Nat \\to Nat");
        term.settle(0.2);
        let line = term.cursor_line();
        assert!(line.contains("#check Nat → Nat"), "{line}");
        assert!(term.line("").contains("Nat → Nat : Type"));
    }

    /// ja: 略記のあとに Tab を押すと、記号に変換して空白を入れない
    /// en: Converts the abbreviation to a symbol without adding a space when Tab is pressed after it
    #[story("C8")]
    #[test]
    fn converts_the_abbreviation_to_a_symbol_without_adding_a_space_when_tab_is_pressed_after_it() {
        // `(· .succ)` では Lean が読めないので、記号のすぐあとに続けられるようにする。
        let mut term = Terminal::start();
        term.type_keys("#check (\\.\t.succ : Nat \\to\t)");
        term.settle(0.2);
        let line = term.cursor_line();
        assert!(line.contains("#check (·.succ : Nat →)"), "{line}");
    }

    /// ja: 略記でない名前のあとで Tab を押すと、名前を補完する
    /// en: Completes the name when Tab is pressed after a name that is not an abbreviation
    #[story("C8", "D1")]
    #[test]
    fn completes_the_name_when_tab_is_pressed_after_a_name_that_is_not_an_abbreviation() {
        let mut term = Terminal::start();
        term.type_keys("#check Nat.suc\t");
        let end = Instant::now() + Duration::from_secs(30);
        while !term.cursor_line().contains("Nat.succ") {
            assert!(Instant::now() < end, "{}", term.cursor_line());
            term.settle(0.2);
        }
    }
}

mod history {
    //! ja: 履歴
    //! en: History
    use super::*;

    /// ja: 複数行のブロックを実行すると、行を改行でつないだ 1 件を履歴に保存する
    /// en: Saves a multi-line block as one history entry with its newlines when the block runs
    #[story("C3", "F3")]
    #[test]
    fn saves_a_multi_line_block_as_one_history_entry_with_its_newlines_when_the_block_runs() {
        let mut term = Terminal::start();
        term.block(&["def merged : Nat -> Nat", "  | 0 => 1", "  | _ => 2"]);
        let entries = term.saved_history();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].matches('\n').count(), 2, "{:?}", entries[0]);
    }

    /// ja: ブロックのあとに Ctrl-P を 1 回押すと全体を呼び出し、Enter で 1 つの入力として送る
    /// en: Recalls the whole block when Ctrl-P is pressed once after a block, and sends it as one input
    #[story("C3")]
    #[test]
    fn recalls_the_whole_block_when_ctrl_p_is_pressed_once_after_a_block_and_sends_it_as_one_input()
    {
        let mut term = Terminal::start();
        term.block(&["def merged : Nat -> Nat", "  | 0 => 1", "  | _ => 2"]);
        term.type_keys("\x10");
        term.settle(0.2);
        // 入力したときの行は画面に残っているので、カーソルのある行とその上を見る。
        let screen = term.screen_text();
        let tail: Vec<&str> = screen.lines().rev().take(3).collect();
        assert!(tail[0].contains("| _ => 2"), "{screen}");
        assert!(tail[2].contains("def merged"), "{screen}");
        // 行単位で送っていたら 1 行目だけがエラーになる。
        assert!(
            term.line_until("", PROMPT, 40)
                .contains("already been declared")
        );
    }

    /// ja: ディレクトリを含まない履歴のパスで入力すると、前回の履歴のあとに追記する
    /// en: Appends to the previous history when input is sent with a history path without a directory
    #[story("F3")]
    #[test]
    fn appends_to_the_previous_history_when_input_is_sent_with_a_history_path_without_a_directory()
    {
        // LEANI_HISTORY=history のような相対パスだと親のディレクトリが "" になる。
        // そのまま作ろうとすると失敗し、履歴の読み込みごとスキップされる。
        // 読み込めていない履歴に 1 行目を書き込むので、前回までの履歴がすべて消える。
        let mut term = Terminal::open(
            80,
            Some("bare-history"),
            "\n# 2026-01-01 00:00:00.000000\n+def old := 1\n",
        );
        term.line("1 + 1");
        assert_eq!(term.saved_history(), ["def old := 1", "1 + 1"]);
    }

    /// ja: 入力のあとで leani を SIGKILL で止めると、その入力が履歴に残っている
    /// en: Keeps the entered lines in history when leani is killed with SIGKILL
    #[story("F3")]
    #[test]
    fn keeps_the_entered_lines_in_history_when_leani_is_killed_with_sigkill() {
        // 入力を確定するたびに追記しているので、どう終了しても履歴は残る。
        let mut term = Terminal::start();
        term.line("1 + 2");
        term.line("3 + 4");
        term.close(true);
        assert_eq!(term.saved_history(), ["1 + 2", "3 + 4"]);
    }
}

mod switching_environments {
    //! ja: 環境の切り替え
    //! en: Switching environments
    use super::*;

    /// ja: :env で環境を切り替えると、プロンプトをその環境に設定したプロンプトに変える
    /// en: Changes the prompt to the one set for the environment when :env switches environments
    #[story("G5")]
    #[test]
    fn changes_the_prompt_to_the_one_set_for_the_environment_when_env_switches_environments() {
        let mut term = Terminal::start();
        term.type_keys(":env wide\r");
        term.wait_prompt(WIDE_PROMPT, 90);
        // 入力の行にも 3.14 があるので、Float の表示の形で見る。
        assert!(
            term.line_until("(3.14 : Float)", WIDE_PROMPT, 60)
                .contains("3.140000")
        );
    }
}

mod external_commands {
    //! ja: 外部コマンド
    //! en: External commands
    use super::*;

    /// ja: 外部コマンドの実行中に Ctrl-C を押すと、^C を表示し、それまでの宣言を残す
    /// en: Shows ^C and keeps earlier declarations when Ctrl-C is pressed during an external command
    #[story("F4")]
    #[test]
    fn shows_c_and_keeps_earlier_declarations_when_ctrl_c_is_pressed_during_an_external_command() {
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

mod pager {
    //! ja: pager
    //! en: Pager
    use super::*;

    /// ja: pager のコマンドが無いときに :abbrev を実行すると、一覧をそのまま表示する
    /// en: Shows the list directly when :abbrev runs with a pager command that does not exist
    #[story("C7")]
    #[test]
    fn shows_the_list_directly_when_abbrev_runs_with_a_pager_command_that_does_not_exist() {
        let missing = "leani-no-such-pager";
        let mut term = Terminal::with(&[("MANPAGER", missing), ("PAGER", missing)], true);
        // 一覧が画面より長いときだけ pager を使う。
        let out = term.line(":abbrev");
        assert!(out.contains("α"), "{out}");
    }
}

mod input_not_from_a_terminal {
    //! ja: 端末でない入力
    //! en: Input not from a terminal
    //!
    //! パイプで渡したとき。
    use super::*;

    /// ja: UTF-8 で読めないバイトの行を含む入力を渡すと、その前後の行を実行する
    /// en: Runs the lines around a line of bytes that are not valid UTF-8 when such input is piped
    #[story("F4")]
    #[test]
    fn runs_the_lines_around_a_line_of_bytes_that_are_not_valid_utf_8_when_such_input_is_piped() {
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

    /// ja: 末尾に空行の無い入力を渡すと、最後のブロックを実行する
    /// en: Runs the last block when the input ends without a blank line
    #[story("F4")]
    #[test]
    fn runs_the_last_block_when_the_input_ends_without_a_blank_line() {
        // 最後のブロックは、空行で確定する前に EOF になる。
        let dir = tempfile::tempdir().expect("tempdir");
        let out = piped(b"#eval\n  41 + 1\n", dir.path(), &[]);
        assert!(shows(&out, "42"), "{out}");
    }

    /// ja: lean ファイルを引数にして leani を起動すると、そのファイルの宣言を読み込む
    /// en: Loads the declarations in a file when leani starts with the file as an argument
    #[story("B4")]
    #[test]
    fn loads_the_declarations_in_a_file_when_leani_starts_with_the_file_as_an_argument() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("given.lean");
        std::fs::write(&path, "def given := 12\n").expect("write");
        let out = piped(b"given + 1\n", dir.path(), &[&path.to_string_lossy()]);
        assert!(out.contains("13"), "{out}");
    }
}
