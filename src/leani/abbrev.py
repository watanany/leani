"""略記表と展開 (純粋)。

`\\to` を `→` にする表。VS Code の Lean 拡張と同じ表記と同じ値を使う。"""

from __future__ import annotations

import json
import unicodedata
from importlib.resources import files
from typing import Final

# 略記表。VS Code の Lean 拡張にある 1857 件のうち 1829 件を使う。値も元の表のまま
# なので、`\d` が δ ではなく ↓ になるような意外な対応もそのままにしてある。
# leani とエディタで対応が違うと、ユーザーはどちらで入力しているかを意識しながら
# 入力することになる。
#   https://github.com/leanprover/vscode-lean4/blob/master/lean4-unicode-input/src/abbreviations.json
#
# 除いたのは 2 種類。`{}` → `{$CURSOR}` のように変換後のカーソル位置を持つ
# 26 件は、置き換えたあとにカーソルを戻す仕組みが必要になる。`\` と `+ ` の 2 件は
# space か Tab で確定する方式では入力できない (`\` は、直前の `\` より後ろをキーと
# して表を検索するのでキーが空になる。`+ ` は、キーの中の空白で先に確定してしまう)。
#
# キーの順に並べてある。293 件が他のキーの接頭辞なので、意味で並べても探しやすく
# はならない。表は Rust 版と共有するので abbrev.json に置く。
ABBREV: Final[dict[str, str]] = json.loads(
    files("leani").joinpath("abbrev.json").read_text(encoding="utf-8")
)


def expand_abbrev(head: str) -> tuple[str, int] | None:
    """
    カーソルの手前にある `\\name` を記号に置き換える。(記号, 消す文字数) を返す。

    確定は space か Tab で行い、キー入力ごとには変換しない。`\\a` `\\all` `\\alpha`
    のようにほかのキーの接頭辞になっているキーが 293 件あり、入力するたびに確定
    すると `\\alpha` を入力できなくなる。space か Tab まで待てば、どのキーを入力した
    のかが一意に決まる。

    表に無ければ None を返す。`\\` のあとを空白まで取り出して、そのまま表を検索
    するので、`\\to)` のように記号が続いた形は変換しない (先に Tab で変換する)。
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

    name は直前の `\\` より後ろで、どれかのキーの接頭辞になっている。`\\` の直後なら
    空文字列を返す。略記の途中でなければ None を返す。`"a\\n".leng` の `\\n".leng` の
    ような文字列のエスケープを略記と読むと、名前を補完しなくなる。
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

    name と一致するキーの記号を先頭に置き、name で始まるほかのキーの記号をキーの
    順に続ける。同じ記号は 1 回だけ返す。name が空なら、候補が多すぎるので返さない。
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

    arg がすべて ASCII なら、arg で始まるキーを返す (先頭の `\\` は無くてもよい)。
    それ以外なら記号とみなして、その記号を入力できるキーを返す。端末では
    `:abbrev \\le` と入力して space を押すと `:abbrev ≤` になるので、どちらの形でも
    調べられるようにする。
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


# 列と列のあいだの空白。
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
