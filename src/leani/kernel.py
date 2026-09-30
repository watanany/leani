"""Jupyter のカーネル (副作用)。

ノートブックのセルを Repl に渡す。`python -m leani.kernel -f {connection_file}` で
起動する。ipykernel は optional dependency (`leani[jupyter]`) なので、このモジュールは
leani/__init__.py から import しない。"""

from __future__ import annotations

import re
import sys
from importlib.metadata import version
from typing import Any

from ipykernel.iostream import OutStream
from ipykernel.kernelapp import IPKernelApp
from ipykernel.kernelbase import Kernel

from leani.abbrev import abbrev_candidates
from leani.config import problem, resolve
from leani.pure import dim, name_start, red
from leani.repl import Repl
from leani.types import START_FAILED, ConfigError, EnvLost

# `\to` のように、カーソルの手前が `\` で始まる略記のとき。
ABBREV_HEAD = re.compile(r"\\([^\s\\]*)$")


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
            start = name_start(before)
            try:
                matches = self.repl.complete_names(before[start:]) if self.repl else []
            except Exception:
                matches = []

        return {
            "status": "ok",
            "matches": matches,
            "cursor_start": start,
            "cursor_end": cursor_pos,
            "metadata": {},
        }

    async def do_shutdown(self, restart: bool) -> dict[str, Any]:
        if self.repl is not None:
            self.repl.close()
        return {"status": "ok", "restart": restart}


if __name__ == "__main__":
    IPKernelApp.launch_instance(kernel_class=LeaniKernel)
