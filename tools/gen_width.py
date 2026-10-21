"""
Rust 版の文字の幅の表 (crates/leani-core/src/width.rs) を生成する。

Rust 版の `cell_width` を Python 版と同じ規則にするため、Python の `unicodedata` から
表を作る。Unicode のバージョンが変わると表も変わるので、バージョンを固定して確かめる。

    uv run python tools/gen_width.py
"""

import unicodedata
from collections.abc import Callable
from pathlib import Path

UNICODE = "15.1.0"
OUT = Path(__file__).parent.parent / "crates/leani-core/src/width.rs"

HEADER = f"""\
//! 端末で表示したときの文字の幅の表 (定数)。
//!
//! Python 版の `cell_width` と同じ規則にするため、Python の `unicodedata`
//! (Unicode {UNICODE}) から生成した。結合文字 (`combining` が 0 以外) は幅 0、
//! East Asian Width が W か F の文字は幅 2。どちらでもない文字は幅 1。
//! 生成し直すときは `uv run python tools/gen_width.py` を実行する。
"""


def ranges(hit: Callable[[str], bool]) -> list[tuple[int, int]]:
    """hit が真になるコードポイントを、連続する範囲 (両端を含む) にまとめる。"""
    out: list[tuple[int, int]] = []
    for cp in range(0x110000):
        match (hit(chr(cp)), out):
            case (True, [*_, (lo, hi)]) if hi == cp - 1:
                out[-1] = (lo, cp)
            case (True, _):
                out.append((cp, cp))
            case _:
                pass
    return out


def table(name: str, rs: list[tuple[int, int]]) -> str:
    rows = "".join(f"    ('\\u{{{lo:04x}}}', '\\u{{{hi:04x}}}'),\n" for lo, hi in rs)
    return f"pub(crate) const {name}: &[(char, char)] = &[\n{rows}];\n"


def main() -> None:
    assert unicodedata.unidata_version == UNICODE, unicodedata.unidata_version
    zero = ranges(lambda c: unicodedata.combining(c) != 0)
    wide = ranges(
        lambda c: (
            unicodedata.combining(c) == 0 and unicodedata.east_asian_width(c) in "WF"
        )
    )
    OUT.write_text(f"{HEADER}\n{table('ZERO', zero)}\n{table('WIDE', wide)}")


if __name__ == "__main__":
    main()
