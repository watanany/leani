"""
テストとユーザーストーリーから SPEC.md を組む。

この REPL が「何ができて、どういう性質を持つか」の一覧が欲しい。書き下ろすと
必ずコードから遅れるので、出所を 2 つだけに絞って生成する。手で書き足す場所は
作らない。

    tests/stories.py   誰の何を助けるか。コードから導けないので、ここが出所
    tests/test_*.py    それを確かめている中身。テスト名がそのまま仕様の文になる

    uv run python tools/spec.py            # SPEC.md を書き直す
    uv run python tools/spec.py --check    # ずれていたら 1 を返す (テストが呼ぶ)

ストーリーとテストは 1:1 ではない。テストの分け方はテストの都合で決まっていて
(落ちる理由が 1 つになるように、速い層に寄せるように)、ストーリーの分け方とは
揃わない。なので `@story(...)` で申告してもらい、ここで突き合わせる。テストが
0 件のストーリーは空欄として出る。それが分かることがこの表の目的。

テスト名の読み替えの規則は 3 つだけ。

    __            ハイフン          Ctrl__C   -> Ctrl-C
    ASCII の間の _  空白             do_by     -> do by
    それ以外の _    詰める           行を_待つ -> 行を待つ

describe の docstring はここには出さない。ストーリー基準で並べ替えると同じ
説明が何度も出るので、あれはテストのそばに置いたままにする。
"""

import ast
import glob
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "tests"))

from stories import GROUPS, STORIES, UNBUILT  # noqa: E402

HEAD = """<!-- tools/spec.py が tests/ から生成する。手で書かない。 -->

# leani の性質一覧

leani が誰の何を助けるか (`tests/stories.py`) に、それを確かめているテストの
名前を並べたもの。テスト名はすべて `uv run pytest` で実際に確かめられている。
増減はストーリーかテストを足すか消すかでしか起きない。

    uv run python tools/spec.py

ストーリー {stories} 件、テスト {tests} 件。括弧の中はテストの居場所
(層 / describe)。
"""
HYPHEN = "\x00"  # __ の置き場。1 文字ずつ見る前に退避しておく


# ---------------------------------------------------------------- 名前を戻す


def joinable(left: str, right: str) -> str:
    """語の継ぎ目に入れるもの。英数字同士なら空白、そうでなければ何も入れない。"""
    ascii_word = (
        left[-1:].isascii()
        and left[-1:].isalnum()
        and right[:1].isascii()
        and right[:1].isalnum()
    )
    return " " if ascii_word else ""


def label(name: str, prefix: str) -> str:
    """関数名を仕様の 1 文に戻す。"""
    body = name[len(prefix) :].replace("__", HYPHEN)
    out = ""
    for i, ch in enumerate(body):
        # 継ぎ目に何を入れるかは前後の文字で決まる。右隣を見ずに "_" 自身を
        # 渡していたので、英数字の間の空白まで消えていた (do_by -> doby)。
        out += joinable(out, body[i + 1 : i + 2]) if ch == "_" else ch
    return out.replace(HYPHEN, "-")


# ------------------------------------------------------------ テストを集める


def marked(fn: ast.FunctionDef) -> list[str]:
    """`@story(...)` に書かれたストーリー番号。付いていなければ空。"""
    out = []
    for dec in fn.decorator_list:
        if isinstance(dec, ast.Call) and getattr(dec.func, "id", "") == "story":
            out += [a.value for a in dec.args if isinstance(a, ast.Constant)]
    return out


def descend(nodes, layer: str, where: str, out: list[dict]) -> None:
    """describe を降りながら it を集める。describe は入れ子になれる。"""
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


def collect() -> list[dict]:
    """テストを 1 件 1 件の辞書にして、ファイル順・出現順で返す。"""
    found: list[dict] = []
    for path in sorted(glob.glob("tests/test_*.py")):
        layer = os.path.basename(path)[len("test_") : -len(".py")]
        with open(path) as f:
            tree = ast.parse(f.read())
        descend(tree.body, layer, layer, found)
    return found


def complaints(tests: list[dict]) -> list[str]:
    """生成する前に止めるべきこと。ストーリーの印の付け忘れと書き間違い。"""
    out = []
    for t in tests:
        if not t["stories"]:
            out.append(f"ストーリーの印が無い: {t['where']} / {t['what']}")
        for sid in t["stories"]:
            if sid not in STORIES:
                out.append(f"tests/stories.py に無い番号: {sid} ({t['what']})")
    for sid in sorted(UNBUILT):
        if sid not in STORIES:
            out.append(f"UNBUILT に居るのに STORIES に無い: {sid}")
    return out


# ------------------------------------------------------------------- 組み立て


def render() -> str:
    tests = collect()
    if bad := complaints(tests):
        raise SystemExit("\n".join(bad))

    by_story = {sid: [] for sid in STORIES}
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
