"""判定と整形 (純粋)。

色付け、応答の読み取り、エンジンのバージョン、完結判定、表示の組み立てを扱う。
入力だけで出力が決まるので、テストは入力と出力だけで書ける。"""

from __future__ import annotations

import json
import re
import signal
import textwrap
from collections.abc import Callable, Sequence
from typing import cast

from leani.places import ENGINE_CACHE, TTY
from leani.queries import BLOCK_OPEN, ERR_POS, INCOMPLETE
from leani.types import (
    CMD,
    COMPLETE,
    ERR,
    MORE,
    TAC,
    TERM,
    EngineError,
    Kind,
    Loogle,
    Message,
    Pos,
    Probe,
    Response,
    Sorry,
    State,
)


# 色を付けるかどうかの判定 (TTY) と、色付きの文字列の組み立ては分けてある。判定は
# 起動時に 1 回だけ行うので、ここから下は入力だけで出力が決まる。
def c(code: str, s: str) -> str:
    return f"\033[{code}m{s}\033[0m" if TTY else s


def red(s: str) -> str:
    return c("31", s)


def yellow(s: str) -> str:
    return c("33", s)


def green(s: str) -> str:
    return c("32", s)


def dim(s: str) -> str:
    return c("2", s)


def lean_str(s: str) -> str:
    """
    Python の文字列を Lean の文字列リテラルにする。
    JSON のエスケープは Lean の \\n \\t \\\\ \\" \\uXXXX と互換。
    """
    return json.dumps(s, ensure_ascii=False)


def lean_strs(xs: Sequence[str]) -> str:
    """文字列の列を Lean の `#[...]` にする。"""
    return "#[" + ", ".join(lean_str(x) for x in xs) + "]"


def shorten(
    chunk: Sequence[str], prefix: str, ns: str, hidden: Sequence[str]
) -> list[str]:
    """
    `ns` を open したとき (`ns` が空なら root) に使える、prefix で始まる短い名前。

    chunk は COMPLETE_QUERY の結果のチャンクの 1 つで、protected な名前は先頭に ! が
    付いている。
    Lean と同じく protected な名前は最後の 1 語だけでは書けないので出さない
    (`open Nat` しても `add_comm` は `Nat.add_comm` にならない)。
    """
    head = f"{ns}." if ns else ""
    out: list[str] = []
    for x in chunk:
        protected = x.startswith("!")
        name = x[1:] if protected else x
        if not name.startswith(head + prefix):
            continue

        short = name[len(head) :]
        if (head and protected and "." not in short) or short in hidden:
            continue
        out.append(short)

    return out


def messages(resp: Response) -> list[Message]:
    return resp.get("messages") or []


def errors(resp: Response) -> list[Message]:
    return [m for m in messages(resp) if m.get("severity") == "error"]


def has_error(resp: Response) -> bool:
    return bool(errors(resp))


def info_text(resp: Response) -> str:
    """info メッセージだけを連結したもの。#eval の出力はここに含まれる。"""
    return "\n".join(
        m.get("data", "") for m in messages(resp) if m.get("severity") == "info"
    )


def sorries(resp: Response) -> list[Sorry]:
    return resp.get("sorries") or []


def comment_depth(line: str, depth: int) -> int:
    """行を読み終えたときのブロックコメントの深さ。Lean の `/- -/` は入れ子。"""
    i = 0
    while i < len(line) - 1:
        two = line[i : i + 2]
        if two == "/-":
            depth += 1
            i += 2
        elif two == "-/" and depth:
            depth -= 1
            i += 2
        else:
            i += 1
    return depth


def scan_header(text: str) -> tuple[list[str], set[int]]:
    """
    先頭のヘッダ領域にある import を、モジュール名と行番号で返す。

    ヘッダ領域は、空行、コメント、`import` が続く範囲。行単位で `import` を
    探すと、doc コメントに書いた `import Foo` を本物の import と取り違える。
    それを :save のヘッダに書き出すと、repl はヘッダ全体を無視して起動するので、
    書き出したファイルは :l でも lean でもエラーになる。import 行を取り除く処理
    でも同じで、コメントの行を消してコメントを壊す。
    """
    mods: list[str] = []
    at: set[int] = set()
    depth = 0

    for n, line in enumerate(text.splitlines()):
        if depth:
            depth = comment_depth(line, depth)
            continue

        one = line.strip()
        head = one.split(None, 1)
        if not one or one.startswith("--"):
            continue
        elif one.startswith("/-"):
            depth = comment_depth(line, 0)
            continue
        elif head[0] == "import" and len(head) == 2 and "/-" not in one:
            mods.append(head[1].strip())
            at.add(n)
            continue
        else:
            break  # ヘッダ領域はここで終わり。以降の import は Lean も認めない

    return mods, at


def import_lines(text: str) -> list[str]:
    """`import X` の X を並べる。:save のヘッダに書き戻すため。"""
    return scan_header(text)[0]


def strip_imports(text: str) -> str:
    """
    import 行を取り除く。既存の環境の上で実行するときに使う。

    import はファイルの先頭にしか書けない。
    """
    at = scan_header(text)[1]
    return "\n".join(line for n, line in enumerate(text.splitlines()) if n not in at)


def clip(line: str, width: int) -> str:
    """長い 1 行を端末の幅に収める。折り返すと、並べたときに行の対応がずれる。"""
    return line if len(line) <= width else line[: width - 1] + "…"


def head_line(src: str, width: int = 60) -> str:
    """宣言 1 件を 1 行で表す。報告に使うので、長ければ切り詰める。"""
    return clip(src.strip().split("\n")[0], width)


def error_text(resp: Response, keep: int = 3) -> str:
    """エラーメッセージを連結したもの。長ければ先頭の数行だけ。"""
    blob = "\n".join(m.get("data", "").strip() for m in errors(resp)).strip()
    return "\n".join(blob.split("\n")[:keep])


def offset(src: str, pos: Pos | None) -> int | None:
    """repl の位置 {"line": 1 始まり, "column": 0 始まり} を文字の位置に変換する。"""
    if not isinstance(pos, dict):
        return None

    line, col = pos.get("line"), pos.get("column")
    if not isinstance(line, int) or not isinstance(col, int):
        return None

    lines = src.split("\n")
    if not 1 <= line <= len(lines) or not 0 <= col <= len(lines[line - 1]):
        return None

    return sum(len(one) + 1 for one in lines[: line - 1]) + col


def splice_sorry(src: str, sy: Sorry, script: str) -> str | None:
    """
    sorry 1 つをタクティクのスクリプトに置き換える。位置が取得できなければ None。

    repl は sorry ごとに pos / endPos を返す。テキストを検索して sorry を探すと、
    コメントや識別子の中の "sorry" にも一致するので、必ず位置で切り出す。
    位置で切り出せば、sorry が 2 つ以上あっても 1 つだけを置き換えられる。
    """
    a, b = offset(src, sy.get("pos")), offset(src, sy.get("endPos"))
    if a is None or b is None or src[a:b] != "sorry":
        return None

    pad = src[src.rfind("\n", 0, a) + 1 : a]  # sorry の行の、sorry までの部分
    if "\n" not in script:
        # 1 行なら sorry のあった桁にそのまま置く。前後の空白は変えない。
        # 変えると ⟨sorry, …⟩ が ⟨ rfl, …⟩ になり、行頭にある sorry は
        # インデントが 1 桁になって by ブロックの外に出てしまう。
        return src[:a] + script + src[b:]
    elif not pad.strip():
        # sorry だけの行。sorry の桁にスクリプトを揃える。
        return src[:a] + hang(script, pad) + src[b:]
    else:
        # 行の途中 (:= by sorry)。次の行に、その行より 2 桁深くインデントして置く。
        deeper = pad[: len(pad) - len(pad.lstrip(" \t"))] + "  "
        return src[:a].rstrip(" \t") + "\n" + deeper + hang(script, deeper) + src[b:]


def hang(script: str, pad: str) -> str:
    """スクリプトの 2 行目以降を pad の桁に揃える。1 行目は呼び出し側が置く。"""
    head, *rest = script.split("\n")
    return "\n".join([head] + [pad + one if one.strip() else one for one in rest])


def has_import(src: str) -> bool:
    """ファイルが自分で import を書いているか。コメントの中のものは数えない。"""
    return bool(import_lines(src))


def first_response(buf: str) -> Response | None:
    """
    受信バッファから最初の完全なレスポンスを取り出す。まだ全部届いていなければ None。

    レスポンスは空行で終わる。JSON の中に空行が含まれることもあるので、空行の位置を
    順に試して、最初にパースできたものを使う。
    """
    for m in re.finditer(r"\n[ \t]*\n", buf):
        head = buf[: m.start()]
        if not head.strip():
            continue
        try:
            got = json.loads(head)
        except json.JSONDecodeError:
            continue

        # repl の応答は必ずオブジェクト。配列や数値が返ってきたらパースできなかった
        # ものとして扱い、次の区切りを試す (呼び出し側は添字でキーを取り出す)。
        if isinstance(got, dict):
            return cast(Response, got)

    return None


def parse_env_lines(text: str) -> dict[str, str]:
    """`KEY=value` の並びを dict にする。lake env のキャッシュを読むため。"""
    out: dict[str, str] = {}
    for line in text.splitlines():
        key, sep, value = line.partition("=")
        if sep:
            out[key] = value

    return out


def signal_name(num: int) -> str:
    """シグナル番号を名前にする。知らない番号はそのまま返す。"""
    try:
        return signal.Signals(num).name
    except ValueError:
        return f"signal {num}"


VERSION = re.compile(r"^v(\d+)\.(\d+)\.(\d+)(?:-rc(\d+))?$")


def toolchain_version(tc: str) -> str:
    """`leanprover/lean4:v4.34.0-rc2` から `v4.34.0-rc2` を取る。"""
    return tc.rpartition(":")[2] or tc


def version_key(tag: str) -> tuple[int, int, int, int] | None:
    """
    バージョンを比較するためのキー。rc は同じバージョンの正式リリースより前になる。

    解釈できない形なら None。
    """
    m = VERSION.match(tag)
    if m is None:
        return None

    major, minor, patch, rc = m.groups()
    return (int(major), int(minor), int(patch), int(rc) if rc else 1 << 30)


def pick_tag(version: str, tags: Sequence[str]) -> str | None:
    """
    そのバージョンに使う repl のタグ。同名のタグがあればそれ、無ければそのバージョン
    以下で一番新しいもの。

    repl のタグは Lean のバージョンと同名だが、patch リリースには付かないことがある
    (v4.33.0 はあるが v4.33.1 は無い)。patch で API は変わらないので、1 つ前の
    タグのソースを目的のバージョンでビルドすれば、ビルドに成功する。
    """
    want = version_key(version)
    if want is None:
        return None
    elif version in tags:
        return version

    below = [(k, t) for t in tags if (k := version_key(t)) is not None and k <= want]
    return max(below)[1] if below else None


def engine_dir(engine: str | None, tc: str) -> str:
    """エンジンのパス。設定で明示されていればそれ、無ければバージョンごとのパス。"""
    if engine is not None:
        return engine

    # バージョン名をそのままディレクトリ名に使うので、パスの区切り文字などは `-` に
    # 置き換える。さらに build_engine はこのディレクトリを作り直す (rmtree する)
    # ので、`..` のように親ディレクトリを指す名前は受け付けない。
    version = toolchain_version(tc)
    name = re.sub(r"[^A-Za-z0-9._-]", "-", version)
    if not name.strip(".-"):
        raise EngineError(f"バージョンの名前として使えない: {version}")
    return f"{ENGINE_CACHE}/{name}"


def err_pos(msg: str | None) -> tuple[int, int]:
    """パーサがエラーになるまでに何行何桁まで読めたか。先まで読めた方を、ユーザーが意図した種類とみなす。"""
    m = ERR_POS.search(msg or "")
    return (int(m.group(1)), int(m.group(2))) if m else (0, 0)


def indented(line: str) -> bool:
    return line[:1] in (" ", "\t")


def continues(line: str) -> bool:
    """
    この行が単独の入力になりえないか。インデント行と、行頭の `|`。
    `|` で始まる command も tactic も Lean には無いので、必ず前の行の続き。
    """
    return indented(line) or line.lstrip().startswith("|")


def block_continues(buf: Sequence[str], src: str) -> bool:
    """完結しているが、続きの行がありうるので空行を待つべきか。"""
    return (len(buf) > 1 and continues(buf[-1])) or BLOCK_OPEN.search(src) is not None


def classify(probe: Probe | None) -> tuple[State, Kind]:
    """
    パーサの応答を (入力の状態, 送り方) に変換する。

    判定できなければ err にし、入力をそのまま送って Lean に本当のエラーを出させる
    (leani の推測でエラーを作らない)。
    """
    match probe:
        case None:
            return ERR, CMD
        case {"cmd": {"ok": True}}:
            return COMPLETE, CMD
        case {"term": {"ok": True}}:
            return COMPLETE, TERM
        case _:
            cmd, term = probe.get("cmd"), probe.get("term")
            cmd_err = cmd.get("err", "") if cmd else ""
            term_err = term.get("err", "") if term else ""
            kind = CMD if err_pos(cmd_err) >= err_pos(term_err) else TERM
            if INCOMPLETE.search(cmd_err) or INCOMPLETE.search(term_err):
                return MORE, kind
            else:
                return ERR, kind


def classify_tac(probe: Probe | None) -> tuple[State, Kind]:
    """証明モードでは tacticSeq として読めるかだけを見る。"""
    match probe:
        case {"tac": {"ok": True}}:
            return COMPLETE, TAC
        case {"tac": {"err": str(err)}} if INCOMPLETE.search(err):
            return MORE, TAC
        case _:
            return ERR, TAC


# `exact?` や `simp?` は結果を "Try this:" として info で返す。スクリプトには
# この提案の中身を使う。`exact?` のままだと、:save したファイルを読み込むたびに
# 検索が実行され、結果も環境によって変わる。提案の先頭には `[apply]` のような
# ラベルが付く。
SUGGESTION_TAG = re.compile(r"^\[[^\]]*\]\s*")

# 提案を最後まで読み取れたかの目安。折り返した行を読み損ねると、必ず括弧の対応が
# 合わなくなる。
PAIRS = {"(": ")", "[": "]", "{": "}", "⟨": "⟩"}

PAINT: dict[str, Callable[[str], str]] = {"error": red, "warning": yellow}


def plain(s: str) -> str:
    return s


def panic_line(resp: Response) -> str | None:
    """エンジンが PANIC を出力していたら、その 1 行目。無ければ None。"""
    for m in messages(resp):
        data = m.get("data") or ""
        if "PANIC at" in data:
            return (data.splitlines() or [""])[0]

    return None


def balanced(src: str) -> bool:
    """括弧が閉じているか。提案を途中で切っていないかの確認に使う。"""
    stack: list[str] = []
    for ch in src:
        if ch in PAIRS:
            stack.append(PAIRS[ch])
        elif ch in PAIRS.values() and (not stack or stack.pop() != ch):
            return False

    return not stack


def try_this(messages_: Sequence[Message] | None) -> str | None:
    """
    "Try this:" の提案を返す。無ければ None。

    提案は pretty printer が 100 桁前後で折り返すので、`simp?` の結果は
    普通に複数行になる。1 行目だけを取ると `simp only [a, b,` のような括弧の
    閉じていないスクリプトになり、しかもそれが「完成した証明」として表示される
    ので、ユーザーは気付けない。改行を含めて返す (Lean のタクティクは複数行でも
    問題ない)。
    """
    for m in messages_ or []:
        data = (m.get("data") or "").strip()
        if not data.startswith("Try this:"):
            continue
        body = textwrap.dedent(data[len("Try this:") :].strip("\n"))
        return SUGGESTION_TAG.sub("", body.strip(), count=1).strip() or None

    return None


def span(
    m: Message, lines: Sequence[str], line_off: int, col_off: int
) -> tuple[int, int, int] | None:
    """
    メッセージの位置を (行, 桁, 幅) にする。ソースの範囲外なら None。

    位置は送ったソース上の座標なので、#eval でラップした分のずれ (line_off /
    col_off) を引いてから、その行の長さに収める。
    """
    pos = m.get("pos") or {}
    ln = pos.get("line", 0) - line_off
    if not 1 <= ln <= len(lines):
        return None

    src_line = lines[ln - 1]
    col = max(0, min(pos.get("column", 0) - col_off, len(src_line)))
    end = m.get("endPos") or {}
    width = 1
    if end.get("line", 0) - line_off == ln:
        width = max(1, end.get("column", 0) - col_off - col)

    return ln, col, min(width, max(1, len(src_line) - col))


# loogle の応答を読む。エラーも検索結果も同じステータス 200 で返るので、`error`
# キーがあるかどうかだけで区別する。
def loogle_text(got: Loogle, keep: int = 10, width: int = 100) -> str:
    """loogle の応答を表示する形にする。1 件 2 行 (名前と型 / どの module か)。"""
    err = got.get("error")
    if err:
        lines = [red(f"loogle: {err}")]
        suggest = got.get("suggestions") or []
        if suggest:
            lines.append(dim("もしかして: " + "  ".join(suggest)))
        return "\n".join(lines)

    hits = got.get("hits") or []
    if not hits:
        return dim("見つからなかった")

    # count は見つかった総数で、hits は loogle が 200 件までに切り詰めたもの。
    # 表示するのはさらにその先頭だけなので、全体の件数には count をそのまま使う。
    total = got.get("count", len(hits))
    shown = hits[:keep]
    head = f"{total} 件" + (f" (先頭 {len(shown)} 件)" if len(shown) < total else "")

    lines = [dim(head)]
    for hit in shown:
        sig = f"{hit.get('name', '?')} :{hit.get('type', '')}".rstrip()
        lines.append(clip(" ".join(sig.split()), width))
        lines.append(dim("  " + hit.get("module", "?")))

    return "\n".join(lines)
