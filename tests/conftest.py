"""テストの土台。

中身は src/leani.py にある。pty 越しに起動するときだけ、PATH から叩くのと
同じ bin/leani を使う。

テストは 3 層に分かれている。下ほど速く、下ほど数を多く持つ。

  test_parsing.py      純関数と読み取りだけ。端末もエンジンも要らない。ミリ秒。
  test_engine.py       Repl を直接叩く。1 テスト 1.5 秒。
  test_completion.py   同上。問い合わせ回数は mocker で数える。
  test_terminal.py     pty 越しに本物の readline を相手にする。

どのテストも Lean 本体だけを import する環境で走る。特定の Lake プロジェクト
に依存しないので、このリポジトリの外へ持って行ってもそのまま動く。

エンジン層では 1 行食わせるごとに INVARIANTS を全部確認する。この REPL は
env のスタックと証明モードを持つ状態機械で、踏んだバグはどれも単発の操作では
なく操作の並びで出た。個別の assert とは別に「どの状態でも成り立つはずのこと」
を毎回見ておくと、想定していない並びでも捕まる。
"""

import contextlib
import fcntl
import io
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

import pytest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REPL = os.path.join(ROOT, "bin", "leani")
sys.path.insert(0, os.path.join(ROOT, "src"))
ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]|\x1b[()][A-Za-z0-9]")
PROMPT = "λ> "
WIDE_PROMPT = "λ+> "

# 設定・履歴・init の場所は import 時に定数になるので、読む前に決める。
_STATE = tempfile.mkdtemp(prefix="leani-test-")
os.environ["LEANI_INIT"] = os.path.join(_STATE, "no-such-init.lean")
os.environ["LEANI_HISTORY"] = os.path.join(_STATE, "history")
os.environ["LEANI_CONFIG"] = os.path.join(_STATE, "config.toml")

# 環境を 2 つ置く。core が既定で、wide は :env の切り替え先。Lake を経由
# しないので起動は 1 秒かからない。
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


import leani  # noqa: E402, I001  (定数を読む前に環境変数を決める)
from stories import STORIES  # noqa: E402


# ------------------------------------------------------------ ストーリーの印


def story(*ids):
    """
    このテストが効くユーザーストーリー (tests/stories.py) を申告する。

    テストの中身にも分け方にも触らない。付けた印は tools/spec.py が SPEC.md
    を組むときだけ読む。1 つのテストが複数のストーリーに効いてよく、1 つの
    ストーリーに複数のテストが効いてよい。ストーリーはテストの目次ではなく、
    テストが足りているかを数える表なので、対応は N:M になる。
    """
    unknown = [i for i in ids if i not in STORIES]
    if unknown:
        raise ValueError(f"tests/stories.py に無いストーリー: {unknown}")

    def mark(fn):
        fn.stories = ids
        return fn

    return mark


# ------------------------------------------------------------------ 不変条件

INVARIANTS = [
    (
        "env のスタックと宣言ログの長さが一致する",
        lambda r: len(r.eng.stack) == len(r.eng.log),
    ),
    (
        # boot が通らなかったときだけ env が無い。そのときは宣言も
        # 「環境に入っている」とは言えないので、log も空でなければならない。
        "環境が無いなら宣言も持たない",
        lambda r: r.eng.env is not None or not r.eng.log,
    ),
    (
        "持ち越した proofState は現在の環境のもの",
        lambda r: r.sorry_env is None or r.sorry_env == r.eng.env,
    ),
    (
        "証明の台本と巻き戻し用スタックの長さが一致する",
        lambda r: r.proof is None or len(r.proof.script) == len(r.proof.stack),
    ),
    (
        "証明モードの proofState は今のエンジンのもの",
        lambda r: r.proof is None or r.proof_gen == r.eng.gen,
    ),
    (
        "エンジンのプロセスが生きている",
        lambda r: r.eng.proc is not None and r.eng.proc.poll() is None,
    ),
    (
        "送り方が決まっているなら入力が溜まっている",
        lambda r: r.ready is None or bool(r.buf),
    ),
    (
        "環境を進めた記録があるなら宣言ログも空でない",
        lambda r: r.last is None or not r.last.advanced or bool(r.eng.log),
    ),
]


class Driver:
    """Repl を直接叩く。1 行食わせるごとに不変条件を確認する。"""

    def __init__(self, env=None):
        with contextlib.redirect_stdout(io.StringIO()):
            self.repl = leani.Repl(leani.resolve(env))
        self.check("起動直後")

    @property
    def engine(self):
        # :env で切り替えると作り直されるので、その都度いまのものを返す。
        return self.repl.eng

    def check(self, after):
        for why, holds in INVARIANTS:
            assert holds(self.repl), f"{after} で不変条件が破れた: {why}"

    def feed(self, *lines):
        """行を順に食わせて、出力をまとめて返す。"""
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            for one in lines:
                self.repl.feed(one)
                self.check(f"{one!r} のあと")
        return ANSI.sub("", out.getvalue())

    def block(self, *lines):
        """複数行を打って空行で確定させる。"""
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


# ------------------------------------------------------------------ 端末


class Terminal:
    """pty の向こうで動いている leani 1 つ。"""

    def __init__(self, history, cols=80, history_env=None):
        self.history = history
        env = dict(
            os.environ,
            TERM="xterm-256color",
            LEANI_HISTORY=history_env or str(history),
        )
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            # lakefile の無い所から起動して、cwd に依存しないことも兼ねて見る。
            os.chdir(_STATE)
            os.execve(sys.executable, [sys.executable, REPL], env)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, cols, 0, 0))
        self.buf = ""
        self.wait_for(PROMPT, 90)

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
        self.buf += chunk.decode("utf-8", "replace")
        return True

    def wait_for(self, needle, timeout=30):
        """needle が出るまで読む。sleep で待たないので速く、かつ決定的。"""
        end = time.time() + timeout
        while needle not in self.screen():
            assert time.time() <= end, f"{needle!r} が来ない。受信:\n{self.screen()}"
            assert self._pump(end - time.time()), (
                f"{needle!r} の前に切れた。受信:\n{self.screen()}"
            )
        out, self.buf = self.screen(), ""
        return out

    def settle(self, quiet=0.2):
        """子が打った分を読み終えて、入力待ちに入るまで待つ。

        macOS の libedit は 1 文字ずつの read() の途中で SIGINT を受けると
        EINTR を握り潰して読み直すので、次の入力が来るまで CPython が割り込みを
        見に行けない。エコーの直後にミリ秒で Ctrl-C を送るとここに落ちる。
        人の指では届かない間隔なので、テスト側で静かになるのを待ってから打つ。
        """
        while True:
            before = len(self.buf)
            self._pump(quiet)
            if len(self.buf) == before:
                return

    def screen(self):
        return ANSI.sub("", self.buf).replace("\r", "")

    def type(self, raw):
        os.write(self.fd, raw.encode())

    def line(self, text, wait=PROMPT, timeout=30):
        self.type(text + "\r")
        return self.wait_for(wait, timeout)

    def block(self, *lines, timeout=40):
        for one in lines:
            self.type(one + "\r")
            self.wait_for("|", 20)
        self.type("\r")
        return self.wait_for(PROMPT, timeout)

    def saved_history(self):
        """libedit の履歴ファイルを素の文字列のリストに戻す。"""
        with open(self.history) as f:
            raw = f.read()
        return [
            line.replace("\\040", " ").replace("\\012", "\n")
            for line in raw.splitlines()
            if line and line != "_HiStOrY_V2_"
        ]

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

    def start(cols=80, history_name=None, seed="_HiStOrY_V2_\n"):
        if history_name is None:
            history, history_env = str(tmp_path / "history"), None
        else:
            # 子は _STATE から起動するので、相対名の履歴はそこに落ちる。
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
    パイプ越しに leani へ食わせて、出たものを全部返す。

    端末が無いときの経路を見るためのもの。stdin が tty でないと入力は
    strict デコードになるので、壊れたバイトの扱いはここでしか出ない。
    """
    env = dict(os.environ, LEANI_HISTORY=os.path.join(str(tmp_path), "history"))
    done = subprocess.run(
        [sys.executable, REPL],
        input=src,
        env=env,
        cwd=_STATE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=timeout,
        check=False,
    )
    return done.stdout.decode("utf-8", "replace")
