"""表示を出す (副作用)。

組み立てた表示を端末に出力する。組み立ての処理は pure にある。"""

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
    """エンジンが PANIC を出力したら、普通の結果として扱わずに警告を表示する。"""
    line = panic_line(resp)
    if line is None:
        return False
    else:
        print(red("エンジンが PANIC した。:restart で再起動したほうが安全"))
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
        # repl がリクエスト全体を拒否した (env や proofState が無い)。messages は
        # 空なので、ここで表示しないと画面に何も表示されない。
        print(red(note.strip()))
        print(dim("  leani が持っている環境がエンジンに無い。:restart で再起動できる"))

    if has_error(resp):
        # エラーになった宣言は環境に追加されていない。その sorry のゴールを表示しても
        # 証明を書けないので、エラーの後ろに役に立たないゴールが並ぶだけになる。
        return

    for i, sy in enumerate(sorries(resp)):
        print(green(f"sorry {i + 1} [proofState {sy.get('proofState')}]"))
        print(textwrap.indent((sy.get("goal") or "").rstrip(), "  "))
