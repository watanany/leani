"""
テストとユーザーストーリーから SPEC.md を生成する。

leani で「何ができて、どういう性質を持つか」の一覧が必要。手で書くと必ずコードより
古くなるので、情報源を 2 つだけに絞って生成する。手で書き足す場所は作らない。

    tests/stories.py   誰の何を助けるか。コードからは分からないので、ここが情報源
    tests/test_*.py    それを確かめているテスト。テスト名がそのまま仕様の文になる

    uv run python tools/spec.py            # SPEC.md を書き直す
    uv run python tools/spec.py --check    # 生成結果と違えば 1 を返す (テストから呼ぶ)

ストーリーとテストは 1:1 ではない。テストの分け方はテストの都合で決まっていて
(失敗する理由が 1 つになるように、なるべく速い層でテストするように)、ストーリーの
分け方とは一致しない。なので各テストに `@story(...)` を付けてもらい、ここで対応を
取る。テストが 0 件のストーリーは空欄として表示される。それが分かることがこの表の
目的。

テスト名を文に戻す規則は 3 つだけ。

    __                     ハイフン   Ctrl__C   -> Ctrl-C
    片側が ASCII の _      空白       do_by     -> do by / IO_の式 -> IO の式
    それ以外の _           詰める     行を_待つ -> 行を待つ

describe の docstring は SPEC.md には出さない。ストーリーごとに並べ替えると同じ
説明が何度も出るので、docstring はテストのそばに置いたままにする。
"""

import ast
import glob
import os
import sys
from typing import TypedDict

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "tests"))

from stories import GROUPS, STORIES, UNBUILT  # noqa: E402

HEAD = """<!-- tools/spec.py が tests/ から生成する。手で編集しない。 -->

# leani の性質一覧

leani が誰の何を助けるか (`tests/stories.py`) に、それを確かめているテストの
名前を並べたもの。テスト名はすべて `uv run pytest` で実際に確かめられている。
項目が増えたり減ったりするのは、ストーリーかテストを追加または削除したときだけ。

    uv run python tools/spec.py

ストーリー {stories} 件、テスト {tests} 件。括弧の中はテストの場所
(層 / describe)。
"""


# ---------------------------------------------------------------- 名前を戻す


def ascii_word(ch: str) -> bool:
    """ASCII の英数字 1 文字か。名前の端では空文字が渡されるので、その場合は False。"""
    return bool(ch) and ch.isascii() and ch.isalnum()


def spaced(part: str) -> str:
    """`_` を単語の区切りとして扱う。前後のどちらかが ASCII の英数字なら空白にする。"""
    out = ""
    for i, ch in enumerate(part):
        if ch != "_":
            out += ch
        elif out and (ascii_word(out[-1:]) or ascii_word(part[i + 1 : i + 2])):
            out += " "
    return out


def label(name: str, prefix: str) -> str:
    """関数名を仕様の 1 文に戻す。"""
    return "-".join(spaced(p) for p in name[len(prefix) :].split("__"))


# ------------------------------------------------------------ テストを集める


class Test(TypedDict):
    """テスト 1 件。`what` は it_ の名前、`where` は「層 / describe」。"""

    what: str
    where: str
    stories: list[str]


def marked(fn: ast.FunctionDef) -> list[str]:
    """`@story(...)` に書かれたストーリー番号。付いていなければ空。"""
    out: list[str] = []
    for dec in fn.decorator_list:
        if isinstance(dec, ast.Call) and getattr(dec.func, "id", "") == "story":
            out += [
                a.value
                for a in dec.args
                if isinstance(a, ast.Constant) and isinstance(a.value, str)
            ]
    return out


def descend(nodes: list[ast.stmt], layer: str, where: str, out: list[Test]) -> None:
    """describe を再帰的にたどって it を集める。describe は入れ子にできる。"""
    for node in nodes:
        if not isinstance(node, ast.FunctionDef):
            continue
        if node.name.startswith("describe_"):
            descend(node.body, layer, label(node.name, "describe_"), out)
        elif node.name.startswith("it_"):
            out.append(
                {
                    "what": label(node.name, "it_"),
                    "where": f"{layer} / {where}",
                    "stories": marked(node),
                }
            )


def collect() -> list[Test]:
    """テストを 1 件ずつ辞書にして、ファイルの順、ファイル内で出てくる順に返す。"""
    found: list[Test] = []
    for path in sorted(glob.glob("tests/test_*.py")):
        layer = os.path.basename(path)[len("test_") : -len(".py")]
        with open(path) as f:
            tree = ast.parse(f.read())
        descend(tree.body, layer, layer, found)
    return found


def complaints(tests: list[Test]) -> list[str]:
    """生成を止める理由。`@story` の付け忘れと、ストーリー番号の書き間違い。"""
    out: list[str] = []
    for t in tests:
        if not t["stories"]:
            out.append(f"@story が無い: {t['where']} / {t['what']}")
        out.extend(
            f"tests/stories.py に無い番号: {sid} ({t['what']})"
            for sid in t["stories"]
            if sid not in STORIES
        )
    out.extend(
        f"UNBUILT にあるのに STORIES に無い: {sid}"
        for sid in sorted(UNBUILT)
        if sid not in STORIES
    )
    return out


# ------------------------------------------------------------------- 組み立て


def render() -> str:
    tests = collect()
    if bad := complaints(tests):
        raise SystemExit("\n".join(bad))

    by_story: dict[str, list[Test]] = {sid: [] for sid in STORIES}
    for t in tests:
        for sid in t["stories"]:
            by_story[sid].append(t)

    out = [HEAD.format(stories=len(STORIES), tests=len(tests))]
    for group, title in GROUPS.items():
        ids = [sid for sid in STORIES if sid.startswith(group)]
        if not ids:
            continue
        out.append(f"## {group}. {title}\n")
        for sid in ids:
            out.append(f"### {sid} {STORIES[sid]}\n")
            if by_story[sid]:
                out.append(
                    "\n".join(f"- {t['what']} ({t['where']})" for t in by_story[sid])
                    + "\n"
                )
            elif sid in UNBUILT:
                out.append("まだ作っていない。\n")
            else:
                out.append("**確かめているテストが無い。**\n")

    return "\n".join(out)


def main(argv: list[str]) -> int:
    want = render()
    if "--check" not in argv:
        with open("SPEC.md", "w") as f:
            f.write(want)
        return 0

    try:
        with open("SPEC.md") as f:
            have = f.read()
    except OSError:
        have = None
    if have == want:
        return 0

    print("SPEC.md が古い: uv run python tools/spec.py", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
