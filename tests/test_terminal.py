"""端末が関わる部分だけ。pty 越しに本物の行編集をテストする。

ここはエンジンの層よりさらに時間がかかるので、端末なしで確認できるものは置かない。
"""

import time

from conftest import WIDE_PROMPT, piped, story


def describe_行編集():

    @story("C3")
    def it_プロンプトの色を桁として数えない(terminal):
        # 数えていると折り返す位置がずれ、履歴から呼び出した行を Backspace で
        # 消せなくなる。80 桁のうちプロンプトが 3 桁を使う。
        term = terminal(cols=80)
        row = term.screen.cursor.y
        term.type("a" * 74)
        term.settle()
        assert term.screen.cursor.y == row, "74 桁で折り返している"
        assert term.screen.cursor.x == 77, term.cursor_line()

    @story("C3")
    def it_履歴から呼び出した行を_Backspace_で修正できる(terminal):
        term = terminal()
        term.line("1 + 100")
        term.type("\x10")  # Ctrl-P
        term.settle()
        assert "1 + 100" in term.cursor_line()
        term.type("\x7f\x7f\x7f" + "1")  # 100 を消して 1 に
        term.settle()
        assert term.cursor_line().rstrip().endswith("1 + 1"), term.cursor_line()
        assert "2" in term.line("")

    @story("F4")
    def it_Ctrl__C_で入力途中の行を捨てる(terminal):
        term = terminal()
        term.type("def half : Nat")
        term.settle()
        assert "def half" in term.cursor_line()
        term.type("\x03")
        assert "^C" in term.wait_prompt()
        assert "2" in term.line("1 + 1"), "入力途中の内容が残っている"

    @story("A4")
    def it_Ctrl__C_で評価中の計算を止めても宣言が残る(terminal, tmp_path):
        term = terminal()
        term.line("def before := 5")
        # 評価が始まってから中断する。評価の前 (完結判定の途中など) に中断すると、
        # 確かめたいエンジンの中断にならない。
        mark = tmp_path / "started"
        term.type(f'#eval do IO.FS.writeFile "{mark}" ""; IO.sleep 60000\r')
        t0 = time.time()
        while not mark.exists():
            assert time.time() - t0 < 30, "評価が始まらない"
            term.settle(0.1)
        term.type("\x03")
        term.wait_prompt(timeout=60)
        assert time.time() - t0 < 30, "中断しても止まらず、評価が最後まで実行された"
        assert "5" in term.line("before")

    @story("C3", "F3")
    def it_捨てた入力途中のブロックも履歴には残る(terminal):
        # Ctrl-C で捨てるのは入力バッファであって、確定した行の記録ではない。
        # 長い宣言の途中で入力を間違えても、ユーザーに全体を入力し直させない。
        term = terminal()
        term.type("def typo : Nat -> Nat\r")
        term.wait_prompt("|", 20)
        term.type("\x03")
        term.wait_prompt()
        term.type("\x10")  # Ctrl-P
        term.settle()
        assert "def typo : Nat -> Nat" in term.cursor_line()


def describe_略記の入力():

    @story("C5")
    def it_space_で記号に変換され_space_も残る(terminal):
        # 確定に使った space を入力から消すと `a \to b` が `a →b` になり、ユーザーは
        # 記号を入力するたびに space を追加で入力することになる。
        term = terminal()
        term.type("#check Nat \\to Nat")
        term.settle()
        assert "#check Nat → Nat" in term.cursor_line(), term.cursor_line()
        assert "Nat → Nat : Type" in term.line("")

    @story("C8")
    def it_Tab_で記号に変換され_空白は入らない(terminal):
        # `(· .succ)` では Lean が読めないので、記号のすぐあとに続けられるようにする。
        term = terminal()
        term.type("#check (\\.\t.succ : Nat \\to\t)")
        term.settle()
        assert "#check (·.succ : Nat →)" in term.cursor_line(), term.cursor_line()

    @story("C8", "D1")
    def it_略記でなければ_Tab_は名前を補完する(terminal):
        term = terminal()
        term.type("#check Nat.suc\t")
        end = time.time() + 30
        while "Nat.succ" not in term.cursor_line():
            assert time.time() < end, term.cursor_line()
            term.settle()


def describe_履歴():

    @story("C3", "F3")
    def it_複数行のブロックは_1_件にまとまる(terminal):
        term = terminal()
        term.block("def merged : Nat -> Nat", "  | 0 => 1", "  | _ => 2")
        entries = term.saved_history()
        assert len(entries) == 1, entries
        assert entries[0].count("\n") == 2, entries[0]

    @story("C3")
    def it_1_回の_Ctrl__P_でブロック全体を呼び出して_1_つの入力として送る(terminal):
        term = terminal()
        term.block("def merged : Nat -> Nat", "  | 0 => 1", "  | _ => 2")
        term.type("\x10")
        term.settle()
        assert "| _ => 2" in term.screen_text()
        assert "def merged" in term.screen_text()
        # 行単位で送っていたら 1 行目だけがエラーになる。
        assert "already been declared" in term.line("", timeout=40)

    @story("F3")
    def it_ディレクトリを含まない履歴のパスでも前回の履歴を消さない(terminal):
        # LEANI_HISTORY=history のような相対パスだと dirname が "" になる。
        # そのまま makedirs("") を呼ぶと例外になり、履歴の読み込みごとスキップされる。
        # 読み込めていない履歴に 1 行目を書き込むので、前回までの履歴がすべて消える。
        term = terminal(
            history_name="bare-history",
            seed="\n# 2026-01-01 00:00:00.000000\n+def old := 1\n",
        )
        term.line("1 + 1")
        assert "def old := 1" in term.saved_history(), "前回の履歴が消えた"

    @story("F3")
    def it_異常終了したセッションの履歴も残る(terminal):
        # 入力を確定するたびに追記しているので、どう終了しても履歴は残る。
        term = terminal()
        term.line("1 + 2")
        term.line("3 + 4")
        term.close(kill=True)
        assert term.saved_history() == ["1 + 2", "3 + 4"]


def describe_環境の切り替え():

    @story("G5")
    def it_プロンプトが設定どおりに変わる(terminal):
        term = terminal()
        term.type(":env wide\r")
        term.wait_prompt(WIDE_PROMPT, 90)
        assert "3.14" in term.line("(3.14 : Float)", wait=WIDE_PROMPT, timeout=60)


def describe_外部コマンド():

    @story("F4")
    def it_Ctrl__C_で外部コマンドを止めてもセッションが残る(terminal):
        # 子プロセスは同じプロセスグループにいるので、Ctrl-C のシグナルは
        # leani にも届く。
        # そこで leani が終了すると、それまでに実行した宣言をすべて失う。
        term = terminal()
        term.line("def keepBang : Nat := 41")
        term.type(":! sleep 30\r")
        term.settle()
        term.type("\x03")
        assert "^C" in term.wait_prompt()
        assert "42" in term.line("keepBang + 1"), "セッションが終了した"


def describe_端末でない入力():
    """
    パイプで渡したとき。cli.main が stdin を errors="replace" でデコードするのは
    ここだけ。
    """

    @story("F4")
    def it_UTF__8_で読めないバイトがあっても後続の行を失わない(tmp_path):
        # strict デコードのままだと UnicodeDecodeError で異常終了するうえ、読み込み
        # 済みの入力も一緒に消えて後続の行まで失われる。leani は読めないバイトを
        # 置き換えて渡し、Lean の構文エラーとして報告させる。
        out = piped(b"def keepPipe : Nat := 41\n\xff\xfe\nkeepPipe + 1\n", tmp_path)
        assert "42" in out, out

    @story("B4")
    def it_ファイルを指定して起動すると宣言を使える(tmp_path):
        path = tmp_path / "given.lean"
        path.write_text("def given := 12\n")
        out = piped(b"given + 1\n", tmp_path, args=[str(path)])
        assert "13" in out, out
