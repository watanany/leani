"""テストの共通部分。

leani のコードは src/leani/ にある。pty 越しに起動するときも、インストールした
leani コマンドと同じものを `python -m leani` として子プロセスで起動する。

テストは 3 層 (5 ファイル) に分かれている。上の層ほど速い。テストの数はエンジンの
層が一番多く、端末の層は一番遅いので数を絞ってある。

  test_parsing.py      純粋関数と読み取りだけ。端末もエンジンも不要。ミリ秒。
  test_engine.py       Repl を直接呼ぶ。1 テストに数秒。
  test_completion.py   同上。問い合わせ回数は mocker で数える。
  test_terminal.py     pty 越しに本物の行編集をテストする。
  test_kernel.py       Jupyter のカーネルを別のプロセスで起動し、セルを送る。

どのテストも Lean 本体だけを import する環境で実行する。特定の Lake プロジェクト
に依存しないので、このリポジトリの外に持ち出してもそのまま実行できる。

エンジン層では 1 行渡すごとに INVARIANTS をすべて確認する。この REPL は
env のスタックと証明モードを持つ状態機械で、バグは単発の操作ではなく操作の組み合わせで
起きやすい。個別の assert とは別に「どの状態でも成り立つはずのこと」
を毎回確認しておくと、想定していない操作の順番でもバグを検出できる。
"""

import contextlib
import fcntl
import io
import json
import os
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import textwrap
import time

import pyte
import pytest
from prompt_toolkit.history import FileHistory

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src")
# インストールした leani と同じエントリポイントを使う。
# -m なら package のまま起動できる。
REPL = ["-m", "leani"]
sys.path.insert(0, SRC)
ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]|\x1b[()][A-Za-z0-9]")
# prompt_toolkit は末尾の空白を書かずにカーソルを動かすので、画面上の
# プロンプトに空白は残らない。
PROMPT = "λ>"
WIDE_PROMPT = "λ+>"
CPR = b"\x1b[6n"  # カーソル位置の問い合わせ
# 読み込んだデータの末尾で途切れたエスケープシーケンス。
# 次に読んだデータと連結してから解釈する。
ESC_TAIL = re.compile(rb"\x1b\[?[0-9;?]*$")

# 設定、履歴、init のパスは import 時に定数になるので、import する前に決める。
_STATE = tempfile.mkdtemp(prefix="leani-test-")
os.environ["LEANI_INIT"] = os.path.join(_STATE, "no-such-init.lean")
os.environ["LEANI_HISTORY"] = os.path.join(_STATE, "history")
os.environ["LEANI_CONFIG"] = os.path.join(_STATE, "config.toml")

# 環境を 2 つ用意する。core がデフォルトで、wide は :env の切り替え先。Lake を経由
# しないので起動は 1 秒もかからない。
with open(os.environ["LEANI_CONFIG"], "w") as _f:
    _f.write(
        textwrap.dedent("""\
        default = "core"

        [env.core]
        imports = []

        [env.wide]
        imports = ["Lean.Elab.Frontend"]
        prompt = "λ+> "
        """)
    )


import leani  # noqa: E402, I001  (定数を読み込む前に環境変数を決める)
from stories import STORIES  # noqa: E402


# ---------------------------------------------------------- ストーリーの指定


def story(*ids):
    """
    このテストが対象にするユーザーストーリー (tests/stories.py) を指定する。

    テストの中身にも分け方にも影響しない。ID が tests/stories.py にあるかは
    import するときに確認する。tools/spec.py は SPEC.md を生成するとき、この指定を
    AST から読む。1 つのテストが複数のストーリーを対象にしてよく、
    1 つのストーリーを複数のテストが対象にしてよい。ストーリーはテストの目次ではなく、
    テストが足りているかを数える表なので、対応は N:M になる。
    """
    unknown = [i for i in ids if i not in STORIES]
    if unknown:
        raise ValueError(f"tests/stories.py に無いストーリー: {unknown}")

    def mark(fn):
        return fn

    return mark


# ------------------------------------------------------------------ 不変条件

INVARIANTS = [
    (
        "env のスタックと宣言ログの長さが一致する",
        lambda r: len(r.eng.stack) == len(r.eng.log),
    ),
    (
        # boot が失敗したときだけ env が無い。そのときは宣言も
        # 「環境に含まれている」とは言えないので、log も空でなければならない。
        "環境が無いなら宣言も持たない",
        lambda r: r.eng.env is not None or not r.eng.log,
    ),
    (
        "持ち越した proofState は今の環境のもの",
        lambda r: r.sorry_env is None or r.sorry_env == r.eng.env,
    ),
    (
        "証明のスクリプトと巻き戻し用スタックの長さが一致する",
        lambda r: r.proof is None or len(r.proof.script) == len(r.proof.stack),
    ),
    (
        "証明モードの proofState は今のエンジンのもの",
        lambda r: r.proof is None or r.proof_gen == r.eng.gen,
    ),
    (
        "エンジンのプロセスが実行中である",
        lambda r: r.eng.proc is not None and r.eng.proc.poll() is None,
    ),
    (
        "送り方が決まっているならバッファに入力がある",
        lambda r: r.ready is None or bool(r.buf),
    ),
    (
        "環境を進めた記録があるなら宣言ログも空でない",
        lambda r: r.last is None or not r.last.advanced or bool(r.eng.log),
    ),
]


class Driver:
    """Repl を直接呼ぶ。1 行渡すごとに不変条件を確認する。"""

    def __init__(self, env=None):
        with contextlib.redirect_stdout(io.StringIO()):
            self.repl = leani.repl.Repl(leani.config.resolve(env))
        self.check("起動直後")

    @property
    def engine(self):
        # :env で切り替えるとエンジンが再起動されるので、その都度今のエンジンを返す。
        return self.repl.eng

    def check(self, after):
        for why, holds in INVARIANTS:
            assert holds(self.repl), f"{after} で不変条件が成り立たなくなった: {why}"

    def feed(self, *lines):
        """行を順に渡して、出力をまとめて返す。"""
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            for one in lines:
                self.repl.feed(one)
                self.check(f"{one!r} のあと")
        return ANSI.sub("", out.getvalue())

    def block(self, *lines):
        """複数行を入力して空行で確定させる。"""
        return self.feed(*lines, "")

    @property
    def declarations(self):
        return self.engine.log

    def close(self):
        self.engine.kill()


@pytest.fixture
def repl():
    driver = Driver()
    yield driver
    driver.close()


def interrupt_after_write(mocker, eng, when):
    """
    when(リクエスト) が真になった最初のリクエストで Ctrl-C を再現する。

    リクエストを書いたあと、レスポンスを読む前に KeyboardInterrupt を raise する。
    実際の Ctrl-C も、たいていエンジンが計算している途中、つまりこの位置で起きる。
    send_cmd で Interrupted を直接 raise すると、Engine.send がプロセスを止める
    処理も、読んでいないレスポンスがパイプに残る状態もテストできない。
    中断したリクエストを返すリストを返す。
    """
    real = eng._exchange
    hit = []

    def exchange(obj):
        if not hit and when(obj):
            hit.append(obj)
            eng.proc.stdin.write(json.dumps(obj) + "\n\n")
            eng.proc.stdin.flush()
            raise KeyboardInterrupt
        else:
            return real(obj)

    mocker.patch.object(eng, "_exchange", side_effect=exchange)
    return hit


# ------------------------------------------------------------------ 端末


class Terminal:
    """pty の先で実行している leani のプロセス 1 つ。

    prompt_toolkit は入力行をカーソル移動で再描画するので、受け取ったバイト列から
    色を取り除いただけでは画面の内容にならない (プロンプトは末尾の空白を書かずに
    カーソルを進めるだけで、入力した文字は消しては書き直される)。そこで pyte で端末を
    再現し、画面そのものを確認する。カーソル位置の問い合わせ (CPR) にも本物の端末と
    同じように応答する。応答しないと prompt_toolkit は 2 秒待ってから警告を出す。

    leani が入力待ちになったかどうかは、カーソルがプロンプトの行にあるかで判定する
    (wait_prompt)。leani が print したものは再描画されないので、受け取った順に
    保存しておき (raw)、プロンプトが表示されたところでまとめて返す。
    """

    ROWS = 24

    def __init__(self, history, cols=80, history_env=None):
        self.history = history
        self.screen = pyte.Screen(cols, self.ROWS)
        self.stream = pyte.ByteStream(self.screen)
        self.raw = ""  # leani が print したもの。色を取り除いて保存する
        self._pending = b""
        env = dict(
            os.environ,
            TERM="xterm-256color",
            LEANI_HISTORY=history_env or str(history),
            PYTHONPATH=SRC,
        )
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            # lakefile の無いディレクトリから起動して、cwd に依存しないことも確認する。
            os.chdir(_STATE)
            os.execve(sys.executable, [sys.executable, *REPL], env)
        fcntl.ioctl(
            self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", self.ROWS, cols, 0, 0)
        )
        self.wait_prompt(PROMPT, 90)

    def _feed(self, chunk):
        """受け取ったバイトを画面に出力する。CPR には今のカーソル位置で応答する。"""
        self._pending += chunk
        cut = ESC_TAIL.search(self._pending)
        head = self._pending[: cut.start()] if cut else self._pending
        self._pending = self._pending[cut.start() :] if cut else b""

        for i, part in enumerate(head.split(CPR)):
            if i:
                # 直前までのバイトを出力したあとのカーソル位置で応答する。
                row, col = self.screen.cursor.y + 1, self.screen.cursor.x + 1
                os.write(self.fd, f"\x1b[{row};{col}R".encode())
            self.stream.feed(part)
            self.raw += ANSI.sub("", part.decode("utf-8", "replace")).replace("\r", "")

    def _pump(self, budget):
        ready, _, _ = select.select([self.fd], [], [], max(0.0, min(0.2, budget)))
        if not ready:
            return True
        try:
            chunk = os.read(self.fd, 65536)
        except OSError:
            return False
        if not chunk:
            return False
        self._feed(chunk)
        return True

    def screen_text(self):
        """今の画面。末尾の空行は取り除く。"""
        return "\n".join(line.rstrip() for line in self.screen.display).rstrip()

    def cursor_line(self):
        return self.screen.display[self.screen.cursor.y]

    def wait_prompt(self, prompt=PROMPT, timeout=30):
        """プロンプトが表示されて入力待ちになるまで待つ。sleep で待たないので速い。

        保存していた出力を返して空にする。次に待つときは、それ以降の出力だけを見る。
        """
        end = time.time() + timeout
        while True:
            self.settle()
            if self.cursor_line().lstrip().startswith(prompt):
                out, self.raw = self.raw, ""
                return out
            assert time.time() <= end, (
                f"{prompt!r} で入力待ちにならない。画面:\n{self.screen_text()}"
            )
            assert self._pump(end - time.time()), (
                f"{prompt!r} の前に出力が途切れた。画面:\n{self.screen_text()}"
            )

    def settle(self, quiet=0.2):
        """子プロセスが出力したものを読み終えるまで待つ。

        画面とカーソルで判定する。再描画はカーソルを動かすだけのことがあるので、
        受け取ったバイト数だけでは「まだ出力が続いている」ことを見逃す。
        """

        def state():
            return (self.screen_text(), self.screen.cursor.y, self.screen.cursor.x)

        while True:
            before = state()
            self._pump(quiet)
            if state() == before:
                return

    def type(self, raw):
        os.write(self.fd, raw.encode())

    def line(self, text, wait=PROMPT, timeout=30):
        self.type(text + "\r")
        return self.wait_prompt(wait, timeout)

    def block(self, *lines, timeout=40):
        for one in lines:
            self.type(one + "\r")
            self.wait_prompt("|", 20)
        self.type("\r")
        return self.wait_prompt(PROMPT, timeout)

    def saved_history(self):
        """履歴ファイルを入力した順のリストにする。leani と同じ実装で読み込む。"""
        return list(reversed(list(FileHistory(self.history).load_history_strings())))

    def close(self, kill=False):
        try:
            if kill:
                os.kill(self.pid, signal.SIGKILL)
            else:
                self.type(":q\r")
                self._pump(3.0)
        except OSError:
            pass
        for step in (lambda: os.close(self.fd), lambda: os.waitpid(self.pid, 0)):
            with contextlib.suppress(OSError):
                step()


@pytest.fixture
def terminal(tmp_path):
    """pty 越しの leani。cols を変えたいときは terminal(cols=100)。"""
    made = []

    def start(cols=80, history_name=None, seed=""):
        if history_name is None:
            history, history_env = str(tmp_path / "history"), None
        else:
            # 子プロセスは _STATE から起動するので、相対パスの履歴は _STATE に作られる。
            history, history_env = os.path.join(_STATE, history_name), history_name
        with open(history, "w") as f:
            f.write(seed)
        term = Terminal(history, cols=cols, history_env=history_env)
        made.append(term)
        return term

    yield start
    for term in made:
        term.close()


def piped(src, tmp_path, timeout=180):
    """
    パイプ越しに leani へ渡して、出力をすべて返す。

    端末が無いときの処理を確認するためのもの。stdin が tty でないと入力は
    strict デコードになるので、壊れたバイトの扱いはここでしか確認できない。
    """
    env = dict(
        os.environ,
        LEANI_HISTORY=os.path.join(str(tmp_path), "history"),
        PYTHONPATH=SRC,
    )
    done = subprocess.run(
        [sys.executable, *REPL],
        input=src,
        env=env,
        cwd=_STATE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=timeout,
        check=False,
    )
    return done.stdout.decode("utf-8", "replace")
