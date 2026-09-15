"""端末が絡むところだけ。pty 越しに本物の行編集を相手にする。

ここは 1 テスト 1.5 秒かかるので、端末なしで確かめられるものは置かない。
"""

from conftest import WIDE_PROMPT, piped, story


def describe_行編集():

    @story("C3")
    def it_プロンプトの色を桁として数えない(terminal):
        # 数えていると折り返す位置がずれ、履歴から戻した行を Backspace で
        # 消せなくなる (実際に踏んだバグ)。80 桁でプロンプト 3 桁ぶんが空く。
        term = terminal(cols=80)
        row = term.screen.cursor.y
        term.type("a" * 74)
        term.settle()
        assert term.screen.cursor.y == row, "74 桁で折り返している"
        assert term.screen.cursor.x == 77, term.cursor_line()

    @story("C3")
    def it_履歴から戻した行を_Backspace_で直せる(terminal):
        term = terminal()
        term.line("1 + 100")
        term.type("\x10")  # Ctrl-P
        term.settle()
        assert "1 + 100" in term.cursor_line()
        term.type("\x7f\x7f\x7f" + "1")  # 100 を消して 1 に
        term.settle()
        assert term.cursor_line().rstrip().endswith("1 + 1"), term.cursor_line()
        assert "2" in term.line("")

    @story("A4", "F4")
    def it_Ctrl__C_で書きかけを捨てる(terminal):
        term = terminal()
        term.type("def half : Nat")
        term.settle()
        assert "def half" in term.cursor_line()
        term.type("\x03")
        assert "^C" in term.wait_prompt()
        assert "2" in term.line("1 + 1"), "書きかけが残っている"

    @story("C3", "F3")
    def it_捨てた書きかけのブロックも履歴には残る(terminal):
        # Ctrl-C で捨てるのは入力バッファであって、確定した行の記録ではない。
        # 長い宣言の途中で打ち間違えても、丸ごと打ち直させない。
        term = terminal()
        term.type("def typo : Nat -> Nat\r")
        term.wait_prompt("|", 20)
        term.type("\x03")
        term.wait_prompt()
        term.type("\x10")  # Ctrl-P
        term.settle()
        assert "def typo : Nat -> Nat" in term.cursor_line()


def describe_履歴():

    @story("C3", "F3")
    def it_複数行のブロックは一件にまとまる(terminal):
        term = terminal()
        term.block("def merged : Nat -> Nat", "  | 0 => 1", "  | _ => 2")
        entries = term.saved_history()
        assert len(entries) == 1, entries
        assert entries[0].count("\n") == 2, entries[0]

    @story("C3")
    def it_一回の_Ctrl__P_で丸ごと戻り丸ごと通る(terminal):
        term = terminal()
        term.block("def merged : Nat -> Nat", "  | 0 => 1", "  | _ => 2")
        term.type("\x10")
        term.settle()
        assert "| _ => 2" in term.screen_text()
        assert "def merged" in term.screen_text()
        # 行単位で送られていたら 1 行目だけがエラーになる。
        assert "already been declared" in term.line("", timeout=40)

    @story("F3")
    def it_ディレクトリ成分の無い履歴でも前回のぶんを消さない(terminal):
        # LEANI_HISTORY=history のように相対名だと dirname が "" になり、
        # makedirs("") が投げて読み込みごと飛ばされていた。読めていない
        # 履歴に 1 行目で書き込むので、前回までのぶんが丸ごと消える。
        term = terminal(
            history_name="bare-history",
            seed="\n# 2026-01-01 00:00:00.000000\n+def old := 1\n",
        )
        term.line("1 + 1")
        assert "def old := 1" in term.saved_history(), "前回の履歴が消えた"

    @story("F3")
    def it_落ちたセッションの履歴も残る(terminal):
        # 確定するたび追記しているので、終わり方に関わらず残る。
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
        # 子は同じプロセスグループにいるので Ctrl-C はこちらにも来る。
        # そこで抜けると、それまでに通した宣言を全部失う。
        term = terminal()
        term.line("def keepBang : Nat := 41")
        term.type(":! sleep 30\r")
        term.settle()
        term.type("\x03")
        assert "^C" in term.wait_prompt()
        assert "42" in term.line("keepBang + 1"), "セッションが畳まれた"


def describe_端末でない入力():
    """パイプで食わせたとき。入力が strict デコードになるのはここだけ。"""

    @story("F4")
    def it_UTF__8_で読めないバイトがあっても後続の行を失わない(tmp_path):
        # strict デコードのままだと UnicodeDecodeError で落ちるうえ、読み込み
        # 済みのぶんが一緒に消えて後続の行まで無くなる。置き換えて渡し、
        # Lean の構文エラーとして報告させる。
        out = piped(b"def keepPipe : Nat := 41\n\xff\xfe\nkeepPipe + 1\n", tmp_path)
        assert "42" in out, out
