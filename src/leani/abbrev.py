"""略記表と展開 (純粋)。

`\\to` を `→` にする表。VS Code の Lean 拡張と同じ表記と同じ値を使う。"""

from __future__ import annotations

import json
import unicodedata
from importlib.resources import files
from typing import Final

# 表の出どころと除いた略記は Rust 版 (crates/leani-core/src/abbrev.rs) に書いてある。
# src/leani/abbrev.json は crates/leani-core/abbrev.json への symlink である。
ABBREV: Final[dict[str, str]] = json.loads(
    files("leani").joinpath("abbrev.json").read_text(encoding="utf-8")
)


def expand_abbrev(head: str) -> tuple[str, int] | None:
    """
    カーソルの手前にある `\\name` を記号に置き換える。(記号, 消す文字数) を返す。

    space か Tab を押したときに呼ぶ。キーを押すたびに呼ばない理由は DECISIONS.md
    にある。
    """
    cut = head.rfind("\\")
    name = head[cut + 1 :]
    sym = ABBREV.get(name) if cut >= 0 else None
    if sym is None:
        return None
    else:
        return sym, len(name) + 1


def abbrev_head(head: str) -> str | None:
    """
    カーソルの手前が `\\name` の途中なら、name を返す。Tab で略記を補完するときに使う。
    """
    cut = head.rfind("\\")
    name = head[cut + 1 :]
    if cut < 0 or not any(k.startswith(name) for k in ABBREV):
        return None
    else:
        return name


def abbrev_candidates(name: str) -> list[str]:
    """
    `\\name` を置き換える記号の候補を返す。端末と Jupyter の Tab 補完で使う。
    """
    if not name:
        return []
    else:
        keys = sorted(k for k in ABBREV if k.startswith(name) and k != name)
        first = [name] if name in ABBREV else []
        return list(dict.fromkeys(ABBREV[k] for k in first + keys))


def abbrev_lookup(arg: str) -> list[tuple[str, str]]:
    """
    `:abbrev` で表示する (キー, 記号) を、キーの順に返す。
    """
    if arg.isascii():
        key = arg.removeprefix("\\")
        return [(k, s) for k, s in ABBREV.items() if k.startswith(key)]
    else:
        return [(k, s) for k, s in ABBREV.items() if s == arg]


def cell_width(text: str) -> int:
    """
    端末で表示したときの幅。全角は 2、結合文字は 0。全角か半角かが決まっていない
    文字は 1 とする。
    """
    return sum(
        0
        if unicodedata.combining(c)
        else 2
        if unicodedata.east_asian_width(c) in "WF"
        else 1
        for c in text
    )


def abbrev_table(pairs: list[tuple[str, str]], width: int) -> str:
    """
    (キー, 記号) を `man ascii` のように複数の列に並べる。縦に並べ、列の下まで行ったら
    右の列に続ける。列の数は、幅に収まる範囲でいちばん多くする。
    """
    cells = [(f"\\{k}", s) for k, s in pairs]
    for n in range(len(cells), 0, -1):
        rows = -(-len(cells) // n)
        cols = [cells[i : i + rows] for i in range(0, len(cells), rows)]
        if fits(cols, width) or n == 1:
            return "\n".join(table_row(cols, r) for r in range(rows))
        else:
            continue
    return ""


def column_widths(col: list[tuple[str, str]]) -> tuple[int, int]:
    """1 列のキーの幅と記号の幅。"""
    return max(len(k) for k, _ in col), max(cell_width(s) for _, s in col)


def fits(cols: list[list[tuple[str, str]]], width: int) -> bool:
    used = sum(k + 1 + s for k, s in map(column_widths, cols)) + GAP * (len(cols) - 1)
    return used <= width


GAP = 3


def table_row(cols: list[list[tuple[str, str]]], r: int) -> str:
    """r 行目。列ごとにキーと記号の幅を揃える。"""
    parts = []
    for col in cols:
        if r < len(col):
            kw, sw = column_widths(col)
            key, sym = col[r]
            parts.append(f"{key:<{kw}} {sym}" + " " * (sw - cell_width(sym)))
        else:
            parts.append("")
    return (" " * GAP).join(parts).rstrip()
