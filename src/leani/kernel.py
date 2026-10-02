"""Jupyter のカーネル (副作用)。

ノートブックのセルを Repl に渡す。`python -m leani.kernel -f {connection_file}` で
起動する。ipykernel は optional dependency (`leani[jupyter]`) なので、このモジュールは
leani/__init__.py から import しない。"""

from __future__ import annotations

import re
import subprocess
import sys
from importlib.metadata import version
from typing import Any

from ipykernel.iostream import OutStream
from ipykernel.kernelapp import IPKernelApp
from ipykernel.kernelbase import Kernel
from prompt_toolkit.completion import CompleteEvent, Completion
from prompt_toolkit.document import Document

from leani.abbrev import abbrev_candidates
from leani.config import problem, resolve
from leani.pure import dim, red
from leani.repl import NameCompleter, Repl, config_envs
from leani.types import START_FAILED, ConfigError, EnvLost

# `\to` のように、カーソルの手前が `\` で始まる略記のとき。
ABBREV_HEAD = re.compile(r"\\([^\s\\]*)$")


def jupyter_matches(line: str, got: list[Completion]) -> tuple[int, list[str]]:
    """
    prompt_toolkit の候補を、Jupyter の形 (置き換えを始める位置と候補) に直す。

    PathCompleter と ExecutableCompleter は、カーソルの後に足す文字 (`leanf` に対する
    `oo`) だけを返す。JupyterLab はそれをそのまま一覧に表示するので、入力済みの部分を
    含めた語 (`leanfoo`) に直す。入力済みの部分は表示用の名前 (display) から分かる。
    ディレクトリは `/` まで入れて、続けて Tab を押せるようにする。Jupyter の候補は
    置き換えを始める位置が 1 つなので、いちばん手前の位置に揃える。
    """
    spans = []
    for c in got:
        cut = len(line) + c.start_position
        shown = c.display_text
        name = shown.removesuffix("/")
        typed = name[: len(name) - len(c.text)] if name.endswith(c.text) else ""
        begin = cut - len(typed) if line[:cut].endswith(typed) else cut
        word = line[begin:cut] + c.text + ("/" if shown.endswith("/") else "")
        spans.append((begin, word))

    start = min((b for b, _ in spans), default=len(line))
    return start, [line[start:b] + w for b, w in spans]


class CellOutput:
    """
    カーネル用の Output。セル 1 つ分の表示を出し、エラーがあったかを記録する。

    ipykernel は sys.stdout と sys.stderr をセルの出力に送るので、ここでは print する
    だけでよい。エンジンを用意するときの表示 (boot) も同じ sys.stdout に出るので、
    順番が入れ替わらない。
    """

    def __init__(self) -> None:
        self.failed = False
        # Jupyter が silent で実行を求めたセル。表示は出さず、エラーだけ記録する。
        self.silent = False

    def write(self, text: str = "", end: str = "\n") -> None:
        if not self.silent:
            print(text, end=end, flush=True)

    def fail(self, text: str) -> None:
        self.failed = True
        if not self.silent:
            print(text, file=sys.stderr, flush=True)

    def shell(self, cmd: str) -> None:
        # 子プロセスの出力は sys.stdout を通らず、カーネルを起動した端末に出る。
        # 受け取ってからセルに表示する。セルからは入力できないので、stdin は空にする
        # (読もうとするコマンドが止まらないように)。終了コードは見ない (端末と同じ)。
        done = subprocess.run(
            cmd,
            shell=True,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            errors="replace",
            check=False,
        )
        if not self.silent:
            print(done.stdout, end="", flush=True)
            print(done.stderr, end="", file=sys.stderr, flush=True)

    def page(self, text: str) -> None:
        # セルの出力はスクロールして読めるので、pager は使わない。
        self.write(text)


class LeaniKernel(Kernel):
    implementation = "leani"
    implementation_version = version("leani")
    language = "lean4"
    language_info = {  # noqa: RUF012 (Kernel の属性を上書きする)
        "name": "lean4",
        "mimetype": "text/x-lean4",
        "file_extension": ".lean",
        "pygments_lexer": "lean4",
    }
    banner = "leani: Lean 4 の対話 REPL"

    def __init__(self, **kwargs: Any) -> None:
        super().__init__(**kwargs)  # type: ignore[no-untyped-call]
        self.out = CellOutput()
        # エンジンは最初のセルで起動する。起動のメッセージとエラーをセルに表示する
        # ため。起動に失敗したら None のままにして、次のセルでもう一度起動する。
        self.repl: Repl | None = None

    def set_parent(self, ident: Any, parent: Any, channel: str = "shell") -> None:
        # print した表示をセルの出力にするには、sys.stdout と sys.stderr にセルの
        # メッセージを知らせる。IPythonKernel はこれを shell で行うが、基底の Kernel
        # は行わないので、ここで行う。
        super().set_parent(ident, parent, channel)  # type: ignore[no-untyped-call]
        if channel == "shell":
            for s in (sys.stdout, sys.stderr):
                if isinstance(s, OutStream):
                    s.set_parent(parent)  # type: ignore[no-untyped-call]

    def open_repl(self) -> Repl | None:
        """設定は端末の leani と同じく、カレントディレクトリから決める。"""
        try:
            cfg = resolve(None, None, [])
        except ConfigError as e:
            self.out.fail(red(str(e)))
            return None

        why = problem(cfg)
        if why:
            self.out.fail(red(why))
            return None

        try:
            return Repl(cfg, out=self.out)
        except START_FAILED as e:
            self.out.fail(red(str(e) or "エンジンが起動しなかった"))
            return None

    def run(self, code: str) -> None:
        """セルの各行を、端末で入力したときと同じ順に Repl に渡す。"""
        if self.repl is None:
            self.repl = self.open_repl()
        repl = self.repl
        if repl is None:
            return

        repl.start_block()
        for line in code.split("\n"):
            if repl.feed(line) == "quit":
                self.out.write(dim(":q はノートブックでは何もしない"))
        repl.end_block()

    async def do_execute(  # type: ignore[override]
        self,
        code: str,
        silent: bool,
        # 下の 3 つは Kernel が渡すが、leani では使わない。
        store_history: bool = True,  # noqa: ARG002
        user_expressions: dict[str, Any] | None = None,  # noqa: ARG002
        allow_stdin: bool = False,  # noqa: ARG002
    ) -> dict[str, Any]:
        self.out.failed = False
        self.out.silent = silent
        try:
            self.run(code)
        except KeyboardInterrupt:
            # Engine.send の外 (整形や子プロセス) で中断した。
            if self.repl is not None:
                self.repl.discard()
            self.out.fail("^C")
        except START_FAILED as e:
            # エンジンを再起動できなかった。次のセルで最初から起動する。
            self.out.fail(red(f"エンジンを再起動できなかった: {e}"))
            self.out.fail(dim("次のセルでエンジンを起動する。それまでの宣言は消える"))
            self.repl = None
        except EnvLost as e:
            # :env で元の環境にも戻れなかった。
            self.out.fail(red(str(e)))
            self.out.fail(dim("次のセルでエンジンを起動する。それまでの宣言は消える"))
            self.repl = None
        except Exception as e:
            self.out.fail(red(f"内部エラー: {type(e).__name__}: {e}"))
            if self.repl is not None:
                self.repl.discard()

        if self.out.failed:
            return {
                "status": "error",
                "execution_count": self.execution_count,
                "ename": "LeanError",
                "evalue": "",
                "traceback": [],
            }
        else:
            return {
                "status": "ok",
                "execution_count": self.execution_count,
                "payload": [],
                "user_expressions": {},
            }

    async def do_complete(self, code: str, cursor_pos: int) -> dict[str, Any]:
        before = code[:cursor_pos]
        m = ABBREV_HEAD.search(before)
        if m:
            # JupyterLab には space で略記を変換する機能が無いので、Tab で変換する。
            start = m.start()
            matches = abbrev_candidates(m[1])
        else:
            # 端末と同じ補完を使う。セルは複数行なので、カーソルのある行だけを渡す。
            head = before.rfind("\n") + 1
            line = before[head:]
            got = list(
                NameCompleter(self.complete_names, config_envs).get_completions(
                    Document(line), CompleteEvent()
                )
            )
            at, matches = jupyter_matches(line, got)
            start = head + at

        return {
            "status": "ok",
            "matches": matches,
            "cursor_start": start,
            "cursor_end": cursor_pos,
            "metadata": {},
        }

    def complete_names(self, prefix: str) -> list[str]:
        """定数名の候補。エンジンが起動していないときと失敗したときは候補を出さない。"""
        try:
            return self.repl.complete_names(prefix) if self.repl else []
        except Exception:
            return []

    async def do_shutdown(self, restart: bool) -> dict[str, Any]:
        if self.repl is not None:
            self.repl.close()
        return {"status": "ok", "restart": restart}


if __name__ == "__main__":
    IPKernelApp.launch_instance(kernel_class=LeaniKernel)
