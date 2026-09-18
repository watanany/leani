"""表示を出す (副作用)。

組み上がった表示を端末に書く。組み立てそのものは pure にある。"""

from __future__ import annotations

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
from leani.types import Response


def die(msg: str) -> NoReturn:
    print(f"leani: {msg}", file=sys.stderr)
    sys.exit(1)


def panic_check(resp: Response) -> bool:
    """エンジンが PANIC を吐いたら黙って結果扱いしない。"""
    line = panic_line(resp)
    if line is None:
        return False
    else:
        print(red("エンジンが PANIC した。:restart で作り直すのが安全。"))
        print(dim(line))
        return True


def render(resp: Response, src: str, line_off: int = 0, col_off: int = 0) -> None:
    """メッセージを GHCi 風に出す。位置があれば該当行とキャレットを添える。"""
    lines = src.splitlines()

    for m in messages(resp):
        sev = m.get("severity", "information")
        paint = PAINT.get(sev, plain)
        data = (m.get("data") or "").rstrip()
        at = span(m, lines, line_off, col_off) if sev in PAINT else None
        if at is None:
            print(paint(data))
        else:
            ln, col, width = at
            head = f"{ln}:{col + 1}"
            print(dim(head) + "  " + lines[ln - 1])
            print(" " * (len(head) + 2 + col) + paint("^" * width))
            print(paint(textwrap.indent(data, "  ")))

    note = resp.get("message")
    if isinstance(note, str) and note.strip():
        # repl がリクエストごと拒否した (env や proofState が無い)。messages には
        # 何も入らないので、ここで出さないと画面が無反応になる。
        print(red(note.strip()))
        print(dim("  エンジンと環境が食い違っている。:restart で作り直せる"))

    if has_error(resp):
        # 通らなかった宣言は環境に入っていない。その sorry の目標を出しても
        # 埋めようがなく、エラーの後ろに読めない目標が並ぶだけ。
        return

    for i, sy in enumerate(sorries(resp)):
        print(green(f"sorry {i + 1} [proofState {sy.get('proofState')}]"))
        print(textwrap.indent((sy.get("goal") or "").rstrip(), "  "))
