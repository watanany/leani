"""端末が絡むところだけ。pty 越しに本物の readline を相手にする。

ここは 1 テスト 1.5 秒かかるので、端末なしで確かめられるものは置かない。
"""
from conftest import PROMPT, WIDE_PROMPT


def describe_行編集():

    def it_プロンプトの色を桁として数えない(terminal):
        # 数えていると折り返す位置がずれ、履歴から戻した行を Backspace で
        # 消せなくなる (実際に踏んだバグ)。80 桁でプロンプト 3 桁ぶんが空く。
        term = terminal(cols=80)
        term.type("a" * 74)
        term.wait_for("a" * 74, 10)
        assert "\x1b[80G" not in term.buf, "74 桁で折り返している"
        assert "\n" not in term.screen(), "74 桁で折り返している"

    def it_履歴から戻した行を_Backspace_で直せる(terminal):
        term = terminal()
        term.line("1 + 100")
        term.type("\x10")                        # Ctrl-P
        term.wait_for("1 + 100", 10)
        term.type("\x7f\x7f\x7f" + "1")          # 100 を消して 1 に
        assert "2" in term.line("")

    def it_Ctrl_C_で書きかけを捨てる(terminal):
        term = terminal()
        term.type("def half : Nat")
        term.wait_for("def half", 10)
        term.type("\x03")
        # ^C とプロンプトは続けて出る。プロンプトまで待たずに打つと、
        # こちらのエコーが先に届いて同期点を取り違える。
        assert "^C" in term.wait_for(PROMPT, 10)
        assert "2" in term.line("1 + 1"), "書きかけが残っている"


def describe_履歴():

    def it_複数行のブロックは一件にまとまる(terminal):
        term = terminal()
        term.block("def merged : Nat -> Nat", "  | 0 => 1", "  | _ => 2")
        entries = term.saved_history()
        assert len(entries) == 1, entries
        assert entries[0].count("\n") == 2, entries[0]

    def it_一回の_Ctrl_P_で丸ごと戻り丸ごと通る(terminal):
        term = terminal()
        term.block("def merged : Nat -> Nat", "  | 0 => 1", "  | _ => 2")
        term.type("\x10")
        assert "def merged" in term.wait_for("| _ => 2", 10)
        # 行単位で送られていたら 1 行目だけがエラーになる。
        assert "already been declared" in term.line("", timeout=40)

    def it_落ちたセッションの履歴も残る(terminal):
        # 毎行書いているので atexit を待たない。
        term = terminal()
        term.line("1 + 2")
        term.line("3 + 4")
        term.close(kill=True)
        assert term.saved_history() == ["1 + 2", "3 + 4"]


def describe_環境の切り替え():

    def it_プロンプトが設定どおりに変わる(terminal):
        term = terminal()
        term.type(":env wide\r")
        term.wait_for(WIDE_PROMPT, 90)
        assert "3.14" in term.line("(3.14 : Float)", wait=WIDE_PROMPT, timeout=60)
