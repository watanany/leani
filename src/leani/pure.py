"""判定と整形 (純粋)。

色付け、応答の読み取り、エンジンのバージョン、完結判定、表示の組み立てを扱う。
入力だけで出力が決まるので、テストは入力と出力だけで書ける。"""

from __future__ import annotations

import json
import re
import signal
import textwrap
from collections.abc import Callable, Sequence
from dataclasses import replace
from typing import cast

from leani.places import COLOR, ENGINE_CACHE
from leani.queries import (
    BLOCK_OPEN,
    CANNOT_EVAL,
    ERR_POS,
    INCOMPLETE,
    NO_REPR,
    NONCOMPUTABLE,
)
from leani.types import (
    CMD,
    COMPLETE,
    ERR,
    MORE,
    TAC,
    TERM,
    EngineError,
    Hit,
    Kind,
    Loogle,
    Message,
    Pos,
    Probe,
    Proof,
    Response,
    Sorry,
    State,
)


# 色を付けるかどうかの判定 (COLOR) と、色付きの文字列の組み立ては分けてある。判定は
# 起動時に 1 回だけ行うので、ここから下は入力だけで出力が決まる。
def c(code: str, s: str) -> str:
    return f"\033[{code}m{s}\033[0m" if COLOR else s


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


COMPLETE_DELIMS = ' \t\n(),[]{};"'


def name_start(text: str) -> int:
    """
    text の末尾にある名前が始まる位置。補完でカーソルの前の名前を切り出すため。

    Lean の名前は `.` を含むので、`.` では区切らない。
    """
    return max(text.rfind(d) for d in COMPLETE_DELIMS) + 1


def name_chunk(prefix: str) -> str:
    """
    定数名をまとめて取得する単位。名前空間があれば最後の `.` まで、無ければ
    先頭 2 文字。

    Mathlib では 1 文字にすると `C` だけで 7.5 万件 (3.4MB) になるので、単位を
    大きくしすぎない。`Nat.` なら 5684 件、`MeasureTheory.` でも 1 万件に収まる。
    """
    return prefix[: prefix.rfind(".") + 1] if "." in prefix else prefix[:2]


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
    found = [
        (x.startswith("!"), x.removeprefix("!")[len(head) :])
        for x in chunk
        if x.removeprefix("!").startswith(head + prefix)
    ]
    return [
        short
        for protected, short in found
        if not (head and protected and "." not in short) and short not in hidden
    ]


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
        one = line.strip()
        head = one.split(None, 1)
        if depth:
            depth = comment_depth(line, depth)
        elif not one or one.startswith("--"):
            continue
        elif one.startswith("/-"):
            depth = comment_depth(line, 0)
        elif head[0] == "import" and len(head) == 2 and "/-" not in one:
            mods.append(head[1].strip())
            at.add(n)
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
    lines = src.split("\n")
    match pos:
        case {"line": int(line), "column": int(col)} if 1 <= line <= len(
            lines
        ) and 0 <= col <= len(lines[line - 1]):
            return sum(len(one) + 1 for one in lines[: line - 1]) + col
        case _:
            return None


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
    else:
        return put_script(src, a, b, script)


def put_script(src: str, a: int, b: int, script: str) -> str:
    """src[a:b] の sorry をスクリプトに置き換える。"""
    pad = src[src.rfind("\n", 0, a) + 1 : a]  # sorry の行の、sorry までの部分
    if "\n" not in script:
        # 1 行なら sorry のあった桁にそのまま置く。前後の空白は変えない。
        # 変えると ⟨sorry, …⟩ が ⟨ rfl, …⟩ になり、行頭にある sorry は
        # インデントが 1 桁になって by ブロックの外に出てしまう。
        return src[:a] + script + src[b:]
    elif not pad.strip():
        # sorry だけの行。sorry の桁にスクリプトをそろえる。
        return src[:a] + hang(script, pad) + src[b:]
    else:
        # 行の途中 (:= by sorry)。次の行に、その行より 2 桁深くインデントして置く。
        deeper = pad[: len(pad) - len(pad.lstrip(" \t"))] + "  "
        return src[:a].rstrip(" \t") + "\n" + deeper + hang(script, deeper) + src[b:]


def hang(script: str, pad: str) -> str:
    """スクリプトの 2 行目以降を pad の桁にそろえる。1 行目は呼び出し側が置く。"""
    head, *rest = script.split("\n")
    return "\n".join([head] + [pad + one if one.strip() else one for one in rest])


def has_import(src: str) -> bool:
    """ファイルが自分で import を書いているか。コメントの中のものは数えない。"""
    return bool(import_lines(src))


def first_response(buf: str) -> Response | None:
    """
    受信バッファから最初の完全なレスポンスを取り出す。まだすべて届いていなければ None。

    レスポンスは空行で終わる。JSON の中に空行が含まれることもあるので、空行の位置を
    順に試して、最初にパースできたものを使う。
    """
    heads = (buf[: m.start()] for m in re.finditer(r"\n[ \t]*\n", buf))
    return next((r for head in heads if (r := as_response(head)) is not None), None)


def as_response(text: str) -> Response | None:
    """
    text をレスポンスとしてパースする。パースできなければ None。

    repl の応答は必ずオブジェクト。配列や数値ならパースできなかったものとして扱い、
    次の区切りを試す (呼び出し側は添字でキーを取り出す)。
    """
    try:
        got = json.loads(text)
    except json.JSONDecodeError:
        return None
    else:
        return cast(Response, got) if isinstance(got, dict) else None


def last_json(out: str | None) -> object | None:
    """
    エンジンの出力の最後の行を JSON としてパースする。出力が空か、最後の行が
    JSON でなければ None。問い合わせは結果を最後の行に出力する。
    """
    lines = (out or "").strip().splitlines()
    if not lines:
        return None
    else:
        try:
            got: object = json.loads(lines[-1])
        except json.JSONDecodeError:
            return None
        else:
            return got


def parse_env_lines(text: str) -> dict[str, str]:
    """`KEY=value` の並びを dict にする。lake env のキャッシュを読むため。"""
    parts = (line.partition("=") for line in text.splitlines())
    return {key: value for key, sep, value in parts if sep}


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


def toolchain_warning(
    own: bool, engine_tc: str, tc: str, engine_dir: str
) -> str | None:
    """
    自分でビルドしたエンジンの toolchain が、使うバージョンと違うときの警告の文。

    leani が用意したエンジン (own が False) は使うバージョンでビルドしてあるので、
    バージョンは必ず一致する。自分でビルドしたエンジンだけ、バージョンが違って
    いたら警告する (leani がビルドし直すことはしない)。engine_tc が空なら、
    エンジンの lean-toolchain が読めなかったので比べない。
    """
    if own and engine_tc and engine_tc != tc:
        return (
            f"警告: toolchain が違う (使うバージョン={tc} / エンジン={engine_tc})。\n"
            f"  cd {engine_dir} && lake build repl"
        )
    else:
        return None


def version_key(tag: str) -> tuple[int, int, int, int] | None:
    """
    バージョンを比較するためのキー。rc は同じバージョンの正式リリースより前になる。

    解釈できない形なら None。
    """
    m = VERSION.match(tag)
    if m is None:
        return None
    else:
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
    else:
        below = [
            (k, t) for t in tags if (k := version_key(t)) is not None and k <= want
        ]
        return max(below)[1] if below else None


def engine_dir(engine: str | None, tc: str) -> str:
    """エンジンのパス。設定で明示されていればそれ、無ければバージョンごとのパス。"""
    if engine is not None:
        return engine
    else:
        return f"{ENGINE_CACHE}/{version_dir(toolchain_version(tc))}"


def version_dir(version: str) -> str:
    """
    バージョンごとのディレクトリの名前。

    バージョン名をそのままディレクトリ名に使うので、パスの区切り文字などは `-` に
    置き換える。さらに build_engine はこのディレクトリを作り直す (rmtree する)
    ので、`..` のように親ディレクトリを指す名前は受け付けない。
    """
    name = re.sub(r"[^A-Za-z0-9._-]", "-", version)
    if name.strip(".-"):
        return name
    else:
        raise EngineError(f"バージョンの名前として使えない: {version}")


def err_pos(msg: str | None) -> tuple[int, int]:
    """パーサがエラーになるまでに何行何桁まで読めたか。先まで読めたほうを、ユーザーが意図した種類とみなす。"""
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


def tactic_step(proof: Proof, state: int, goals: Sequence[str], tactic: str) -> Proof:
    """タクティクを 1 つ進めた Proof。前の proofState とゴールは :undo 用に積む。"""
    return replace(
        proof,
        state=state,
        goals=tuple(goals),
        script=(*proof.script, tactic),
        stack=(*proof.stack, (proof.state, proof.goals)),
    )


def tactic_undo(proof: Proof) -> Proof | None:
    """最後のタクティクを取り消した Proof。取り消せるタクティクが無ければ None。"""
    match proof.stack:
        case (*rest, (state, goals)):
            return replace(
                proof,
                state=state,
                goals=goals,
                script=proof.script[:-1],
                stack=tuple(rest),
            )
        case _:
            return None


def plain(s: str) -> str:
    return s


def panic_line(resp: Response) -> str | None:
    """
    エンジンが PANIC を出力していたら、PANIC の行。無ければ None。

    同じメッセージの中で、PANIC の前に #eval の出力が、後にバックトレースが続く
    ことがあるので、メッセージの 1 行目ではなく PANIC の行を探す。
    """
    lines = (ln for m in messages(resp) for ln in (m.get("data") or "").splitlines())
    return next((ln for ln in lines if "PANIC at" in ln), None)


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
    datas = ((m.get("data") or "").strip() for m in messages_ or [])
    data = next((d for d in datas if d.startswith("Try this:")), None)
    if data is None:
        return None
    else:
        body = textwrap.dedent(data[len("Try this:") :].strip("\n"))
        return SUGGESTION_TAG.sub("", body.strip(), count=1).strip() or None


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
    else:
        src_line = lines[ln - 1]
        col = max(0, min(pos.get("column", 0) - col_off, len(src_line)))
        end = m.get("endPos") or {}
        width = (
            max(1, end.get("column", 0) - col_off - col)
            if end.get("line", 0) - line_off == ln
            else 1
        )
        return ln, col, min(width, max(1, len(src_line) - col))


# loogle の応答を読む。エラーも検索結果も同じステータス 200 で返るので、`error`
# キーがあるかどうかだけで区別する。
def loogle_text(got: Loogle, keep: int = 10, width: int = 100) -> str:
    """loogle の応答を表示する形にする。1 件 2 行 (名前と型 / どの module か)。"""
    match got:
        case {"error": str(err)} if err:
            suggest = got.get("suggestions") or []
            also = [dim("もしかして: " + "  ".join(suggest))] if suggest else []
            return "\n".join([red(f"loogle: {err}"), *also])
        case {"hits": [_, *_] as hits}:
            return loogle_hits(hits, got.get("count", len(hits)), keep, width)
        case _:
            return dim("見つからなかった")


def loogle_hits(hits: Sequence[Hit], total: int, keep: int, width: int) -> str:
    """
    検索結果の先頭 keep 件を表示する形にする。

    total は見つかった総数で、hits は loogle が 200 件までに切り詰めたもの。
    表示するのはさらにその先頭だけなので、全体の件数には total をそのまま使う。
    """
    shown = hits[:keep]
    head = f"{total} 件" + (f" (先頭 {len(shown)} 件)" if len(shown) < total else "")
    return "\n".join(
        [dim(head), *(row for hit in shown for row in hit_rows(hit, width))]
    )


def hit_rows(hit: Hit, width: int) -> list[str]:
    """検索結果 1 件を 2 行 (名前と型 / どの module か) にする。"""
    sig = f"{hit.get('name', '?')} :{hit.get('type', '')}".rstrip()
    return [clip(" ".join(sig.split()), width), dim("  " + hit.get("module", "?"))]


def not_evaluable_reason(blob: str) -> str | None:
    """#eval のエラー文から、評価できない理由。評価できないエラーでなければ None。"""
    if NONCOMPUTABLE.search(blob):
        return "noncomputable なので評価できない"
    elif NO_REPR.search(blob):
        return "値を表示する Repr や ToString が無いので評価できない"
    elif CANNOT_EVAL.search(blob):
        return "評価できない"
    else:
        return None
