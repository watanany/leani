"""表示を出す (副作用)。

組み立てた表示を Output に出す。組み立ての処理は pure にある。"""

from __future__ import annotations

import subprocess
import sys
import textwrap
from typing import NoReturn

from leani.pure import (
    PAINT,
    dim,
    green,
    has_error,
    messages,
    panic_line,
    plain,
    red,
    sorries,
    span,
)
from leani.types import Output, Response


class Console:
    """端末用の Output。`leani | tee log` で log に残すため、エラーも stdout に出す。"""

    def write(self, text: str = "", end: str = "\n") -> None:
        print(text, end=end, flush=True)

    def fail(self, text: str) -> None:
        print(text, flush=True)

    def shell(self, cmd: str) -> None:
        # 出力を受け取らず、端末につないだまま実行する。vim のような対話するコマンドも
        # 使えるように。
        subprocess.run(cmd, shell=True, check=False)


def die(msg: str) -> NoReturn:
    print(f"leani: {msg}", file=sys.stderr)
    sys.exit(1)


def panic_check(out: Output, resp: Response) -> bool:
    """
    エンジンが PANIC を出力したら、普通の結果として扱わずに警告を表示する。

    利用者が入力したもの (式、do の読み直し、宣言、タクティク) の応答だけを調べる。
    :l、init、replay の応答は調べない。利用者が意図して panic! を書いたファイルも
    あるので、そのファイルの読み込みを失敗にしないため。
    """
    line = panic_line(resp)
    if line is None:
        return False
    else:
        out.fail(red("エンジンが PANIC した。:restart で再起動したほうが安全"))
        out.write(dim(line))
        return True


def render(
    out: Output, resp: Response, src: str, line_off: int = 0, col_off: int = 0
) -> None:
    """メッセージを GHCi 風に出す。位置があれば該当行とキャレットを添える。"""
    lines = src.splitlines()

    for m in messages(resp):
        sev = m.get("severity", "information")
        paint = PAINT.get(sev, plain)
        data = (m.get("data") or "").rstrip()
        at = span(m, lines, line_off, col_off) if sev in PAINT else None
        # error のメッセージは fail に出す。位置の行とキャレットも含めて 1 回で出す。
        emit = out.fail if sev == "error" else out.write
        if at is None:
            emit(paint(data))
        else:
            ln, col, width = at
            head = f"{ln}:{col + 1}"
            emit(
                "\n".join(
                    [
                        dim(head) + "  " + lines[ln - 1],
                        " " * (len(head) + 2 + col) + paint("^" * width),
                        paint(textwrap.indent(data, "  ")),
                    ]
                )
            )

    note = resp.get("message")
    if isinstance(note, str) and note.strip():
        # repl がリクエスト全体を拒否した (env や proofState が無い)。messages は
        # 空なので、ここで表示しないと画面に何も表示されない。
        out.fail(red(note.strip()))
        out.write(
            dim("  leani が持っている環境がエンジンに無い。:restart で再起動できる")
        )

    if has_error(resp):
        # エラーになった宣言は環境に追加されていない。その sorry のゴールを表示しても
        # 証明を書けないので、エラーの後ろに役に立たないゴールが並ぶだけになる。
        return

    for i, sy in enumerate(sorries(resp)):
        out.write(green(f"sorry {i + 1} [proofState {sy.get('proofState')}]"))
        out.write(textwrap.indent((sy.get("goal") or "").rstrip(), "  "))
