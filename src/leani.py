#!/usr/bin/env python3
"""
leani — Lean 4 の対話 REPL。

エンジンは leanprover-community/repl。あれは JSON in / JSON out の機械向け
プロトコルなので、ここが人間向けの層を持つ。

設計上の要点:

* **完結判定は Lean のパーサに投げる。** 「入力が終わったか」を正規表現で
  近似すると、想定外の構文が来るたびに早く確定しすぎるか次の行を飲み込む。
  `Parser.runParserCategory` を現在の環境で走らせて command / term として
  読めるかを聞く。実行はしない。ユーザ定義の notation も効く。
* **エンジンが落ちても続く。** 受理した宣言のログを持ち、落ちたら再起動して
  replay する。評価中の Ctrl-C も同じ経路で復帰する。
* **セッションはソースで持ち出す。** repl には環境を pickle する機能があるが、
  戻した環境で `#eval` すると Lean のコンパイラが PANIC する。`:save` が
  受理した宣言を `.lean` として書き出し、`:l` でも lean 本体でも読める。

読み方: 副作用の有無で節を分けてある。見出しの札はこの 3 つ。

    純粋      入力だけで出力が決まる。同じ入力なら常に同じ結果
    読み取り  ファイルや環境変数を見るが、何も書き換えない
    副作用    プロセス・端末・ファイルを触る。上から下へ流れとして読む

判定と整形はすべて純粋な側に置いてあるので、テストは入力と出力だけで書ける。
状態を持つのは repl プロセス 1 個を抱える `Engine` と、入力バッファと証明モードを
持つ `Repl` の 2 つだけ。
"""

from __future__ import annotations

import atexit
import codecs
import contextlib
import hashlib
import json
import os
import re
import select
import shutil
import signal
import subprocess
import sys
import textwrap
import time
import tomllib
from collections.abc import Callable, Sequence
from dataclasses import dataclass, field
from typing import Any, Final, Literal, NamedTuple, NoReturn, TypeVar

# ---------------------------------------------------------------- 型 (純粋)

# repl とやりとりする JSON。キーは repl 側の都合で決まるので dict のまま扱い、
# 別名で意味だけ持たせる。
Json = dict[str, Any]
Message = Json  # {"severity": ..., "data": ..., "pos": ..., "endPos": ...}
Sorry = Json  # {"proofState": ..., "goal": ...}
Response = Json  # repl の返事ひとつ

Kind = Literal["cmd", "term", "tac"]  # 送り方
State = Literal["complete", "more", "err"]  # 入力の状態
Step = Literal["probe", "done", "quit"]  # 1 行食べたあと何をするか

CMD: Final[Kind] = "cmd"
TERM: Final[Kind] = "term"
TAC: Final[Kind] = "tac"
COMPLETE: Final[State] = "complete"
MORE: Final[State] = "more"
ERR: Final[State] = "err"

T = TypeVar("T")

# -------------------------------------------------------- 置き場所 (読み取り)

HOME = os.path.expanduser("~")
CONFIG_HOME = os.environ.get("XDG_CONFIG_HOME", f"{HOME}/.config")
STATE = os.environ.get("XDG_STATE_HOME", f"{HOME}/.local/state") + "/leani"
CONFIG = os.environ.get("LEANI_CONFIG", f"{CONFIG_HOME}/leani/config.toml")
HIST = os.environ.get("LEANI_HISTORY", f"{STATE}/history")
INIT = os.environ.get("LEANI_INIT", f"{CONFIG_HOME}/leani/init.lean")
# エンジンは leanprover-community/repl。この REPL 本体ではない。無ければ leani が
# 取ってきてビルドする (ensure_engine)。使う Lean の版ごとに掘る。
ENGINE_REPO = "https://github.com/leanprover-community/repl"
ENGINE_CACHE = f"{STATE}/engine"
# 明示されたときは leani は何も管理せず、そのディレクトリをそのまま使う。
ENGINE = os.environ.get("LEANI_ENGINE")
NO_SETUP = bool(os.environ.get("LEANI_NO_SETUP"))
# エンジンの用意で外に聞くときの上限。網が黒穴でも黙って止まらないように。
SETUP_TIMEOUT = 120
# git に認証を聞き返させない。capture_output だと聞かれても見えないまま止まる。
SETUP_ENV = dict(os.environ, GIT_TERMINAL_PROMPT="0")
PROMPT = "λ> "
# 完結判定と補完のクエリが Lean.Parser / CoreM / Json を使うので、ユーザの
# import が何であれこれだけは要る。import は先頭に並べる決まりなので、
# 常に 1 行目に足しておけば他の import と共存できる。
PROBE_IMPORT = "import Lean\n"

# 起動したヘッダが本当に効いたかを見るための 1 行。repl は解決できない import
# を**エラーも出さずに**ヘッダごと捨てて新しい env を返すので、これが通るかで
# しか判別できない。import Lean まで落ちるため、指すのはその中の定数にする
# (完結判定が使っているものそのまま)。
BOOT_PROBE = "#check @Lean.Parser.runParserCategory\n"

COMPLETE_CAP = 40000

# --------------------------------------------------------------- 色 (読み取り)

TTY = sys.stdout.isatty()


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


# \001 / \002 を解釈するのは readline なので、無いときは埋め込めない。
# libedit は \001..\002 の中身をプロンプトの先頭にまとめて吐くので、
# 色のリセットをプロンプトの末尾に置けない (どちらも _setup_readline で立てる)。
RL_OK = False
RL_HOIST = False


def pc(code: str, s: str) -> str:
    """
    プロンプト用の色付け。

    readline (macOS は libedit) はプロンプトの表示幅を数えて折り返し位置と
    カーソル位置を決める。色コードをそのまま置くとそのバイトまで桁として
    数えるので、`\x1b[36mλ> \x1b[0m` は 3 桁なのに 13 桁と見なされ、
    10 桁ずれる。長い行 (履歴から呼び戻した行など) で折り返すと libedit の
    モデルと実際のカーソルが食い違い、Backspace が別のセルを消して文字が
    画面に残る。非表示部分は \001 / \002 で囲んで幅から除く。

    libedit は非表示区間を順番どおりに、ただしすべてプロンプトの先頭へ
    まとめて出す。リセットを末尾に置くと色を出した直後に戻ってしまうので、
    libedit では開きだけを埋め込み、input() を抜けたところで戻す
    (read_line)。入力中の行にも色が乗る。
    """
    if not TTY:
        return s
    if not RL_OK:
        return c(code, s)

    open_ = f"\001\033[{code}m\002"
    return open_ + s if RL_HOIST else open_ + s + "\001\033[0m\002"


# ------------------------------------------------------------- 小道具 (純粋)


def lean_str(s: str) -> str:
    """
    Python の文字列を Lean の文字列リテラルにする。
    JSON のエスケープは Lean の \\n \\t \\\\ \\" \\uXXXX と互換。
    """
    return json.dumps(s, ensure_ascii=False)


def messages(resp: Response) -> list[Message]:
    return resp.get("messages") or []


def errors(resp: Response) -> list[Message]:
    return [m for m in messages(resp) if m.get("severity") == "error"]


def has_error(resp: Response) -> bool:
    return bool(errors(resp))


def info_text(resp: Response) -> str:
    """info メッセージだけを繋いだもの。#eval の出力はここに来る。"""
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

    ヘッダ領域は空行・コメント・`import` が続く範囲。行単位で
    `import` を探すと、doc コメントに書いた `import Foo` を本物と取り違える。
    それを :save のヘッダに書けば、repl はヘッダを丸ごと捨てて起動するので、
    書き出したファイルは :l でも lean でも通らない。落とす側も同じで、
    コメントの行を消してコメントを壊す。
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
    """import 行を落とす。既にある環境へ重ねるとき用 (import は先頭にしか置けない)。"""
    at = scan_header(text)[1]
    return "\n".join(line for n, line in enumerate(text.splitlines()) if n not in at)


def head_line(src: str, width: int = 60) -> str:
    """宣言 1 件を 1 行で指す。報告に使うので長ければ切る。"""
    line = src.strip().split("\n")[0]
    return line if len(line) <= width else line[: width - 1] + "…"


def error_text(resp: Response, keep: int = 3) -> str:
    """エラーメッセージを繋いだもの。長ければ頭だけ。"""
    blob = "\n".join(m.get("data", "").strip() for m in errors(resp)).strip()
    return "\n".join(blob.split("\n")[:keep])


def offset(src: str, pos: Json | None) -> int | None:
    """repl の {"line": 1 から, "column": 0 から} を文字位置に直す。"""
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
    sorry 1 個をタクティクの台本に差し替える。位置が読めなければ None。

    repl は sorry ごとに pos / endPos を返す。テキストを数えて当てると
    コメントや識別子の中の "sorry" に当たるので、必ず位置で切る。数えて
    当てていたころは sorry が 2 個以上あると埋め戻しを丸ごと諦めていた。
    """
    a, b = offset(src, sy.get("pos")), offset(src, sy.get("endPos"))
    if a is None or b is None or src[a:b] != "sorry":
        return None

    pad = src[src.rfind("\n", 0, a) + 1 : a]  # sorry の行の、sorry までの部分
    if "\n" not in script:
        # 1 行なら sorry のあった桁にそのまま置く。前後の空白は動かさない。
        # 動かすと ⟨sorry, …⟩ が ⟨ rfl, …⟩ になり、行頭に寄っている sorry は
        # インデントが 1 桁に潰れて by ブロックから外れる。
        return src[:a] + script + src[b:]
    elif not pad.strip():
        # sorry だけの行。その桁を台本の桁にする。
        return src[:a] + hang(script, pad) + src[b:]
    else:
        # 行の途中 (:= by sorry)。by の下にぶら下げる。
        deeper = re.match(r"[ \t]*", pad).group() + "  "
        return src[:a].rstrip(" \t") + "\n" + deeper + hang(script, deeper) + src[b:]


def hang(script: str, pad: str) -> str:
    """台本の 2 行目以降を pad の桁に揃える。1 行目は呼ぶ側が置く。"""
    head, *rest = script.split("\n")
    return "\n".join([head] + [pad + one if one.strip() else one for one in rest])


def has_import(src: str) -> bool:
    """ファイルが自分で import を書いているか。コメントの中のものは数えない。"""
    return bool(import_lines(src))


def first_response(buf: str) -> Response | None:
    """
    受信バッファから最初の完全なレスポンスを取る。まだ揃っていなければ None。

    レスポンスは空行で終わる。JSON の中に空行が来ることもあるので、空行の候補を
    順に試して最初に読めたものを採る。
    """
    for m in re.finditer(r"\n[ \t]*\n", buf):
        head = buf[: m.start()]
        if not head.strip():
            continue
        try:
            return json.loads(head)
        except json.JSONDecodeError:
            continue

    return None


def parse_env_lines(text: str) -> dict[str, str]:
    """`KEY=value` の並びを dict にする。lake env のキャッシュを読むため。"""
    out: dict[str, str] = {}
    for line in text.splitlines():
        key, sep, value = line.partition("=")
        if sep:
            out[key] = value

    return out


# ------------------------------------------------------- エンジンの版 (純粋)

VERSION = re.compile(r"^v(\d+)\.(\d+)\.(\d+)(?:-rc(\d+))?$")


def toolchain_version(tc: str) -> str:
    """`leanprover/lean4:v4.34.0-rc2` から `v4.34.0-rc2` を取る。"""
    return tc.rpartition(":")[2] or tc


def version_key(tag: str) -> tuple[int, int, int, int] | None:
    """版の並び。rc は同じ版の正式版より前。読めない形なら None。"""
    m = VERSION.match(tag)
    if m is None:
        return None

    major, minor, patch, rc = m.groups()
    return (int(major), int(minor), int(patch), int(rc) if rc else 1 << 30)


def pick_tag(version: str, tags: Sequence[str]) -> str | None:
    """
    その版に使う repl のタグ。同名があればそれ、無ければ以下で一番新しいもの。

    repl のタグは Lean の版と同名だが、patch 版には付かないことがある
    (v4.33.0 はあるが v4.33.1 は無い)。patch で API は変わらないので、1 つ前の
    タグのソースを目的の版でビルドすれば通る。
    """
    want = version_key(version)
    if want is None:
        return None
    elif version in tags:
        return version

    below = [(k, t) for t in tags if (k := version_key(t)) is not None and k <= want]
    return max(below)[1] if below else None


def engine_dir(engine: str | None, tc: str) -> str:
    """置き場所。設定で明示されていればそれ、無ければ版ごとの置き場所。"""
    if engine is not None:
        return engine

    # 版名がそのままディレクトリ名になるので、区切り文字は落とす。さらに
    # build_engine はこの場所を作り直す (rmtree する) ので、`..` のように
    # 上へ抜ける名前は通さない。
    version = toolchain_version(tc)
    name = re.sub(r"[^A-Za-z0-9._-]", "-", version)
    if not name.strip(".-"):
        raise EngineError(f"版の名前として使えない: {version}")
    return f"{ENGINE_CACHE}/{name}"


# ---------------------------------------------------------- 設定 (読み取り)


class ConfigError(Exception):
    """設定が読めない / 指定された環境が無い。"""


@dataclass(frozen=True)
class EnvConfig:
    """
    どの Lake プロジェクトの上で何を import して起動するか。

    ghci の ~/.ghci、ipython の profile と同じ位置づけ。特定のプロジェクト名を
    コードに持たないため、名前は設定ファイル (config.toml) と CLI 引数だけに置く。
    設定が無くても cwd の Lake プロジェクトから推測して動く。

    決まったら変わらないので frozen。作るときは値を整える make を通す。
    """

    name: str
    project: str | None = None
    imports: tuple[str, ...] = ()
    prompt: str = PROMPT
    engine: str | None = None  # None なら leani が用意する

    @staticmethod
    def make(
        name: str,
        project: str | None = None,
        imports: Sequence[str] = (),
        prompt: str | None = None,
        engine: str | None = None,
    ) -> EnvConfig:
        return EnvConfig(
            name=name,
            project=abspath(project),
            imports=tuple(imports),
            prompt=prompt or PROMPT,
            engine=abspath(engine or ENGINE),
        )

    @property
    def header(self) -> str:
        """設定された import 行。:save したファイルの先頭にそのまま書ける形。"""
        return "".join(f"import {m}\n" for m in self.imports)

    @property
    def boot_header(self) -> str:
        return PROBE_IMPORT + self.header

    def __str__(self) -> str:
        mods = " ".join(["Lean", *self.imports])
        return f"{self.name} (import {mods})"


def abspath(path: str | None) -> str | None:
    return os.path.abspath(os.path.expanduser(path)) if path else None


def as_str(value: Any, where: str) -> str | None:
    """設定の文字列 1 つ。型が違えば断る (traceback にしない)。"""
    if value is None or isinstance(value, str):
        return value
    else:
        raise ConfigError(
            f"{where} は文字列で書く (今は {type(value).__name__}): {CONFIG}"
        )


def as_imports(value: Any, where: str) -> Sequence[str]:
    """
    import の並び。

    文字列 1 つを黙って受けると 1 文字ずつの import になり、どれも解決でき
    ないのでヘッダが丸ごと捨てられる (import Lean ごと消える)。断るほうがいい。
    """
    if value is None:
        return ()
    elif isinstance(value, str):
        raise ConfigError(f'{where} は配列で書く: ["{value}"] ({CONFIG})')
    elif isinstance(value, list) and all(isinstance(m, str) for m in value):
        return value
    else:
        raise ConfigError(f"{where} は文字列の配列で書く: {CONFIG}")


def as_table(value: Any, where: str) -> Json:
    """設定の表 1 つ。型が違えば断る。"""
    if value is None:
        return {}
    elif isinstance(value, dict):
        return value
    else:
        raise ConfigError(f"{where} は表で書く (今は {type(value).__name__}): {CONFIG}")


def load_config(path: str = CONFIG) -> Json:
    if not os.path.isfile(path):
        return {}

    try:
        with open(path, "rb") as f:
            return tomllib.load(f)
    except (OSError, tomllib.TOMLDecodeError) as e:
        raise ConfigError(f"設定が読めない: {path}\n  {e}") from e


def lake_root(start: str) -> str | None:
    """lakefile を持つ一番近い親ディレクトリ。無ければ None。"""
    d = os.path.abspath(start)
    while True:
        if any(os.path.isfile(f"{d}/{f}") for f in ("lakefile.toml", "lakefile.lean")):
            return d
        up = os.path.dirname(d)
        if up == d:
            return None
        d = up


def lake_libs(project: str) -> list[str]:
    """lakefile が公開しているライブラリ名。設定が無いときの import 候補。"""
    toml = f"{project}/lakefile.toml"
    if os.path.isfile(toml):
        try:
            with open(toml, "rb") as f:
                data = tomllib.load(f)
        except (OSError, tomllib.TOMLDecodeError):
            return []

        return [lib["name"] for lib in data.get("lean_lib", []) if "name" in lib]

    try:
        with open(f"{project}/lakefile.lean") as f:
            text = f.read()
    except OSError:
        return []

    return re.findall(r"^\s*lean_lib\s+«?([A-Za-z0-9_.\']+)»?", text, re.M)


def problem(cfg: EnvConfig) -> str | None:
    """
    この環境で起動できない理由。無ければ None。

    起動時と :env の切り替え時の両方で使う。切り替えでは今のエンジンを
    落とす前に呼ぶので、駄目なら何も壊さずに断れる。見るだけで何も変えない。
    """
    # 版が違うエンジンでは olean が読めず repl が起動直後に落ちる。
    proj_tc = read_toolchain(cfg.project)
    eng_tc = read_toolchain(cfg.engine) if cfg.engine else None

    reasons = [
        (
            bool(cfg.project) and not os.path.isdir(str(cfg.project)),
            f"Lake プロジェクトが無い: {cfg.project}",
        ),
        (not shutil.which("elan"), "elan が PATH に無い"),
        (not shutil.which("lake"), "lake が PATH に無い"),
        # エンジンを leani が用意する場合の依存。取りに行く前に見ておく。
        (
            cfg.engine is None and not shutil.which("git"),
            f"git が PATH に無い。エンジン ({ENGINE_REPO}) の取得に使う",
        ),
        # 明示されたエンジンは leani が面倒を見ないので、揃っているかだけ見る。
        (
            cfg.engine is not None and not os.path.isdir(cfg.engine),
            f"指定されたエンジンが無い: {cfg.engine}",
        ),
        # 版を合わせる先がこれしかない。無いと elan の既定に落ちて黙って壊れる。
        (
            cfg.engine is not None
            and os.path.isdir(cfg.engine)
            and not os.path.isfile(f"{cfg.engine}/lean-toolchain"),
            f"指定されたエンジンに lean-toolchain が無い: {cfg.engine}",
        ),
        (
            cfg.engine is not None
            and not os.path.isfile(f"{cfg.engine}/.lake/build/bin/repl"),
            f"repl が未ビルド: cd {cfg.engine} && lake build repl",
        ),
        # 起動してから落ちるだけなので、警告ではなく断る。
        (
            eng_tc is not None and proj_tc is not None and eng_tc != proj_tc,
            f"エンジンの版がプロジェクトと違う "
            f"(プロジェクト={proj_tc} / エンジン={eng_tc})。\n"
            f"  {proj_tc} で建て直す: cd {cfg.engine} && "
            f"echo {proj_tc} > lean-toolchain && lake build repl\n"
            f"  または LEANI_ENGINE を外して leani に用意させる",
        ),
    ]
    return next((why for bad, why in reasons if bad), None)


def from_config(
    table: Json,
    name: str,
    project: str | None,
    imports: Sequence[str],
    engine: str | None,
) -> EnvConfig:
    """名前で選んだ環境。書いてある通りに使う (cwd は見ない)。"""
    if name not in table:
        known = " ".join(sorted(table)) or "(ひとつも無い)"
        raise ConfigError(f"環境 {name} は設定に無い: {CONFIG}\n  ある環境: {known}")

    entry = as_table(table[name], f"env.{name}")
    return EnvConfig.make(
        name,
        project=project or as_str(entry.get("project"), f"env.{name}.project"),
        imports=imports or as_imports(entry.get("imports"), f"env.{name}.imports"),
        prompt=as_str(entry.get("prompt"), f"env.{name}.prompt"),
        engine=as_str(entry.get("engine"), f"env.{name}.engine") or engine,
    )


def guess_env(
    project: str | None, imports: Sequence[str], engine: str | None
) -> EnvConfig:
    """
    設定に無いときの推測。cwd から lakefile を持つ親を探し、その lean_lib を
    import する。Lake プロジェクトの外なら Lean 本体だけで起動する。
    """
    root = project if project is not None else lake_root(os.getcwd())
    mods = imports or (lake_libs(root) if root else ())
    return EnvConfig.make(
        os.path.basename(root) if root else "plain",
        project=root,
        imports=mods,
        engine=engine,
    )


def resolve(
    name: str | None = None,
    project: str | None = None,
    imports: Sequence[str] | None = None,
    cfg: Json | None = None,
) -> EnvConfig:
    """
    CLI 引数と設定ファイルから、起動する環境を 1 つ決める。

    優先順は 引数 > 名前付き環境 > default > cwd の Lake プロジェクト。
    """
    conf = load_config() if cfg is None else cfg
    table = as_table(conf.get("env"), "env")
    engine = as_str(conf.get("engine"), "engine")
    mods = tuple(imports or ())
    if name is None and not mods and not project:
        name = as_str(conf.get("default"), "default")

    if name is not None:
        return from_config(table, name, project, mods, engine)
    else:
        return guess_env(project, mods, engine)


# --------------------------------------------- エンジンに投げるクエリ (定数)

# 完結判定。command と term の両方を 1 往復で聞く。
PARSE_PROBE = r"""open Lean Parser in
#eval show CoreM Unit from do
  let src := %s
  let env ← getEnv
  let probe : Name → Json := fun cat =>
    match runParserCategory env cat src with
    | .ok _ => Json.mkObj [("ok", Json.bool true)]
    | .error e => Json.mkObj [("ok", Json.bool false), ("err", Json.str e)]
  IO.println (Json.mkObj [("cmd", probe `command), ("term", probe `term),
                          ("tac", probe `tacticSeq)]).compress"""

COMPLETE_QUERY = r"""open Lean in
#eval show CoreM Unit from do
  let env ← getEnv
  let mut ns : Array String := #[]
  for (n, _) in env.constants.toList do
    if n.isInternalDetail then continue
    let last := n.getString!
    if last.startsWith "_" || last.startsWith "eq_" || last.startsWith "match_"
       || last.startsWith "proof_" || last.startsWith "congr_"
       || last == "go" || last == "loop" || last == "induct" || last == "fun_cases"
       || last == "eq_def" || last == "sizeOf_spec" then continue
    let s := n.toString
    if %s.isPrefixOf s then ns := ns.push s
  IO.println (String.intercalate " " (ns.qsort.toList.take %d))"""

DOC_QUERY = r"""open Lean in
#eval show CoreM Unit from do
  match ← findDocString? (← getEnv) `%s with
  | some d => IO.println d
  | none => pure ()"""

# パーサが「まだ続きがある」と言っているとみなすメッセージ。
INCOMPLETE = re.compile(r"unexpected end of input|unterminated (comment|string)")
# ブロック中でも脱出できるようにする。Lean のソースが行頭 : で始まることは無い
# (`:=` の継続はインデントされる)。
META_LINE = re.compile(r"^:[A-Za-z!?{}]")
# `structure P where` や `induction n with` は Lean 文法ではそれ自体で完結する。
# パーサは「終わり」と言うが、続きのブロックを書きたいのが普通なので確定を遅らせる。
# 完結判定を覆すわけではないので、外しても Enter が 1 回余分に要るだけで済む。
BLOCK_OPEN = re.compile(r"(?:^|[\s)\]}])(where|with|do|by)[ \t]*$")
ERR_POS = re.compile(r"<input>:(\d+):(\d+):")
# 自分で通した宣言の名前。補完に足すためだけなので、取りこぼしても害はない。
DECL_NAME = re.compile(
    r"^\s*(?:@\[[^\]]*\]\s*)*"
    r"(?:private\s+|protected\s+|noncomputable\s+|partial\s+|unsafe\s+|scoped\s+)*"
    r"(?:def|abbrev|theorem|lemma|instance|structure|inductive|class|opaque|axiom)"
    r"\s+([A-Za-z_\u00c0-\uffff][^\s:({\[]*)",
    re.M,
)

# #eval できない式。型だけでも出したほうが親切なので #check に落とす。
NOT_EVALUABLE = re.compile(
    r"noncomputable|failed to compile"
    r"|could not synthesize.*(Repr|ToString|ToExpr|Eval)|cannot evaluate",
    re.S | re.I,
)

# ------------------------------------------------------------- 完結判定 (純粋)


def err_pos(msg: str | None) -> tuple[int, int]:
    """エラーが何行何桁まで進めたか。深く進めた側がユーザの意図した種類。"""
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


def classify(probe: Json | None) -> tuple[State, Kind]:
    """
    パーサの返事を (入力の状態, 送り方) に読む。

    判定できなければ err にして、そのまま投げて Lean に本当のエラーを出させる
    (自前の推測でエラーを作らない)。
    """
    match probe:
        case None:
            return ERR, CMD
        case {"cmd": {"ok": True}}:
            return COMPLETE, CMD
        case {"term": {"ok": True}}:
            return COMPLETE, TERM
        case _:
            cmd_err = (probe.get("cmd") or {}).get("err", "")
            term_err = (probe.get("term") or {}).get("err", "")
            kind = CMD if err_pos(cmd_err) >= err_pos(term_err) else TERM
            if INCOMPLETE.search(cmd_err) or INCOMPLETE.search(term_err):
                return MORE, kind
            else:
                return ERR, kind


def classify_tac(probe: Json | None) -> tuple[State, Kind]:
    """証明モードでは tacticSeq として読めるかだけを見る。"""
    match probe:
        case {"tac": {"ok": True}}:
            return COMPLETE, TAC
        case {"tac": {"err": str(err)}} if INCOMPLETE.search(err):
            return MORE, TAC
        case _:
            return ERR, TAC


# ------------------------------------------------- ファイルを読む (読み取り)


def read_text(path: str) -> str | None:
    """読めなければ None。無い / 権限が無いのどちらでも同じ扱いでよい所で使う。"""
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        return None


def history_entries(path: str) -> bool:
    """
    履歴ファイルに記録が入っているか。

    readline の読み込みが失敗しただけでは、壊れているのか空なのか分からない
    (libedit は中身の無いファイルでも errno を返す)。上書きして失うものが
    あるかどうかは、こちらで直に見て判断する。
    """
    raw = read_text(path)
    if raw is None:
        return False
    return any(line.strip() not in ("", "_HiStOrY_V2_") for line in raw.splitlines())


# ------------------------------------------------------- 起動の用意 (副作用)

LEAN_VERSION = re.compile(r"version (\d+\.\d+\.\d+(?:-rc\d+)?)")


class EngineError(Exception):
    """エンジンを用意できない。起動を断る理由になるが、REPL は落とさない。"""


def read_toolchain(path: str | None) -> str | None:
    """そのディレクトリの lean-toolchain。無ければ None。"""
    text = read_text(f"{path}/lean-toolchain") if path else None
    return text.strip() or None if text else None


def local_toolchain() -> str | None:
    """elan が今選んでいる版。プロジェクトも指定も無いときの落とし所。"""
    try:
        r = subprocess.run(
            ["lean", "--version"],
            capture_output=True,
            text=True,
            check=False,
            timeout=SETUP_TIMEOUT,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None

    m = LEAN_VERSION.search(r.stdout)
    return f"leanprover/lean4:v{m.group(1)}" if m else None


def guess_toolchain(cfg: EnvConfig) -> str | None:
    """
    使う Lean の版。プロジェクト → 明示されたエンジン → elan の既定。

    2 番目は手動で指したエンジン用。leani はそれをビルドし直さないので、
    合わせるべき版は「そのエンジンをビルドした版」しかない。leani が用意した
    エンジンなら engine が None なので、ここは飛ばして既定版に落ちる。
    """
    return (
        read_toolchain(cfg.project) or read_toolchain(cfg.engine) or local_toolchain()
    )


def toolchain(cfg: EnvConfig) -> str:
    """
    使う版を決める。分からなければ断る。

    lean は cwd の lean-toolchain を見て版を決めるので、プロジェクトの外から
    呼ぶと既定の版が選ばれて olean が読めなくなる。ここで決めた版を
    elan run で固定し、エンジンもその版でビルドする。
    """
    tc = guess_toolchain(cfg)
    if tc is None:
        raise EngineError("Lean の版が分からない (lean --version が答えない)")

    return tc


def manual_setup(path: str, tc: str, tag: str | None) -> str:
    """
    自動で用意できなかったときに出す手順。

    タグを省くと HEAD が来て版が合わないので、必ず指す。使う版で
    ビルドし直すところまで含めて、ensure_engine と同じことを手でやる形。
    """
    branch = tag or f"<{toolchain_version(tc)} 以下で一番新しいタグ>"
    return (
        f"手で用意する場合:\n"
        f"  git clone --branch {branch} {ENGINE_REPO} {path}\n"
        f"  echo {tc} > {path}/lean-toolchain\n"
        f"  cd {path} && lake build repl"
    )


def fetch_tags() -> list[str]:
    """repl のタグ一覧。clone せずに聞く。"""
    try:
        r = subprocess.run(
            ["git", "ls-remote", "--tags", "--refs", ENGINE_REPO],
            capture_output=True,
            text=True,
            errors="replace",
            check=False,
            timeout=SETUP_TIMEOUT,
            env=SETUP_ENV,
        )
    except subprocess.TimeoutExpired as e:
        raise EngineError(f"repl のタグを聞くのに {SETUP_TIMEOUT} 秒かかった") from e
    except OSError as e:
        raise EngineError(f"git を呼べなかった: {e}") from e
    except KeyboardInterrupt as e:
        raise EngineError("^C 中断した") from e

    if r.returncode != 0:
        raise EngineError(f"repl のタグが取れなかった:\n{r.stderr.strip()}")

    # 1 行が "<sha>\trefs/tags/v4.33.0"。タグ名だけ取る。
    return [
        ref.rpartition("/")[2]
        for ref in r.stdout.split()
        if ref.startswith("refs/tags/")
    ]


def run_setup(cmd: Sequence[str], cwd: str | None = None) -> None:
    """エンジンの用意に使う外部コマンド。出力はそのまま見せる。"""
    print(dim(f"  {' '.join(cmd)}"), flush=True)
    try:
        r = subprocess.run(list(cmd), cwd=cwd, check=False, env=SETUP_ENV)
    except OSError as e:
        raise EngineError(f"{cmd[0]} を呼べなかった: {e}") from e
    except KeyboardInterrupt as e:
        # 同じプロセスグループなので Ctrl-C はこちらにも来る。断るだけにする。
        raise EngineError(f"^C 中断した: {' '.join(cmd)}") from e

    if r.returncode != 0:
        raise EngineError(f"失敗した (exit {r.returncode}): {' '.join(cmd)}")


def build_engine(path: str, tc: str, tag: str) -> None:
    """
    タグのソースを使う版でビルドして置く。

    olean を読めるかはビルドに使った Lean の版で決まるので、clone した
    lean-toolchain を書き換えてからビルドする。通ってから os.replace で置くので、
    途中で止めても半端なものが残らない。
    """
    version = toolchain_version(tc)
    note = "" if tag == version else f" (タグ {tag} を {version} でビルドする)"
    print(dim(f"エンジンを用意する: {version}{note} — 初回のみ"), flush=True)

    # leani を 2 つ同時に起動しても衝突しないよう、置き場はプロセスごとに分ける。
    tmp = f"{path}.{os.getpid()}.tmp"
    os.makedirs(os.path.dirname(path), exist_ok=True)
    try:
        run_setup(["git", "clone", "--depth", "1", "--branch", tag, ENGINE_REPO, tmp])
        with open(f"{tmp}/lean-toolchain", "w") as f:
            f.write(tc + "\n")

        run_setup(["lake", "build", "repl"], cwd=tmp)
        if not os.path.isfile(f"{tmp}/.lake/build/bin/repl"):
            raise EngineError(f"ビルドしたのに repl が無い: {tmp}")

        # 待っている間に別の leani が置いたなら、動いているそれを消さない。
        if os.path.isfile(f"{path}/.lake/build/bin/repl"):
            print(dim(f"別に用意されていた: {path}"), flush=True)
            return

        shutil.rmtree(path, ignore_errors=True)
        os.replace(tmp, path)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    print(dim(f"用意した: {path}"), flush=True)


def ensure_engine(engine: str | None, tc: str, asked: bool = False) -> str:
    """
    使うエンジンのディレクトリ。leani が持つぶんは無ければ用意する。

    engine が明示されているときは leani の管理外なので、揃っているかを見る
    だけで何も作らないし消さない。build_engine は置き場を作り直すので、
    人が指したディレクトリに向けてはならない。
    """
    path = engine_dir(engine, tc)
    if os.path.isfile(f"{path}/.lake/build/bin/repl"):
        return path
    elif engine is not None:
        raise EngineError(f"repl が未ビルド: cd {path} && lake build repl")
    elif NO_SETUP and not asked:
        raise EngineError(
            f"エンジンが無い: {path}\n"
            f"  LEANI_NO_SETUP が立っているので自動で用意しない "
            f"(leani --setup で用意する)\n{manual_setup(path, tc, None)}"
        )

    tag = None
    try:
        version = toolchain_version(tc)
        tag = pick_tag(version, fetch_tags())
        if tag is None and version_key(version) is None:
            # nightly や stable。版として読めないので比べようがない。repl は
            # master が最新の Lean に追いているので、そこで試す。
            tag = "master"
            print(dim(f"{version} に対応するタグは無い。master で試す"), flush=True)
        elif tag is None:
            raise EngineError(f"repl に {version} 用のタグが無い")

        build_engine(path, tc, tag)
    except EngineError as e:
        # 自動で駄目でも手でなら通ることがある。使うタグまで出しておく。
        raise EngineError(f"{e}\n{manual_setup(path, tc, tag)}") from e

    return path


LAKE_ENV_KEYS = ("LEAN_PATH", "LEAN_SRC_PATH", "DYLD_LIBRARY_PATH", "LD_LIBRARY_PATH")


def lake_env(project: str | None, tc: str = "") -> dict[str, str]:
    """
    lake env が解決する環境変数。

    lake env は起動に 1 秒近くかかるので、新しいキャッシュがあれば使い回す。
    LEAN_PATH は core の olean を指すので、toolchain ごとに別の鍵で持つ。
    同じ鍵で持ち回していたころは、rc を差し替えると前の版の olean を指した
    ままになり、repl は起動するのに import が丸ごと落ちていた。
    """
    if not project:
        return {}

    key = hashlib.sha1(f"{project}\n{tc}".encode()).hexdigest()[:12]
    cache = f"{STATE}/lake-env/{os.path.basename(project)}-{key}"
    if lake_env_stale(cache, project):
        write_lake_env(cache, project)

    return parse_env_lines(read_text(cache) or "")


# キャッシュより新しければ取り直す。manifest は依存の版、lean-toolchain は
# core の版。どちらが動いても LEAN_PATH は変わる。
LAKE_ENV_INPUTS = ("lake-manifest.json", "lean-toolchain")


def lake_env_stale(cache: str, project: str) -> bool:
    if not os.path.isfile(cache):
        return True

    try:
        age = os.path.getmtime(cache)
        return any(
            os.path.getmtime(f"{project}/{name}") > age
            for name in LAKE_ENV_INPUTS
            if os.path.isfile(f"{project}/{name}")
        )
    except OSError:
        # 見ている間に消されることがある。分からなければ取り直す。
        return True


def write_lake_env(cache: str, project: str) -> None:
    script = "\n".join(f'printf "{k}=%s\\n" "${{{k}-}}"' for k in LAKE_ENV_KEYS)
    try:
        r = subprocess.run(
            ["lake", "env", "sh", "-c", script],
            cwd=project,
            capture_output=True,
            text=True,
            errors="replace",
            check=False,
            timeout=SETUP_TIMEOUT,
        )
    except subprocess.TimeoutExpired as e:
        raise EngineError(
            f"lake env が {SETUP_TIMEOUT} 秒で返らなかった ({project})"
        ) from e
    except OSError as e:
        raise EngineError(f"lake env を呼べなかった ({project}): {e}") from e

    if r.returncode != 0:
        raise EngineError(f"lake env が失敗した ({project}):\n{r.stderr.strip()}")

    # ここもプロセスごとに分ける。同時に起動した別の leani と書き合わない。
    tmp = f"{cache}.{os.getpid()}.tmp"
    try:
        os.makedirs(os.path.dirname(cache), exist_ok=True)
        with open(tmp, "w") as f:
            f.write(r.stdout)

        os.replace(tmp, cache)
    except OSError as e:
        raise EngineError(f"lake env を控えられなかった ({cache}): {e}") from e
    finally:
        with contextlib.suppress(OSError):
            os.unlink(tmp)


def prepare(cfg: EnvConfig) -> None:
    """
    その環境で起動できるようにする。エンジンが無ければここで用意する。

    Engine を作る前に呼べる。:env の切り替えでは今のエンジンを落とす前に
    通すので、ここで断れたぶんは何も壊さずに済む。lake env も先に解決して
    キャッシュしておく (Engine の中で失敗させない)。
    """
    tc = toolchain(cfg)
    ensure_engine(cfg.engine, tc)
    lake_env(cfg.project, tc)


# ------------------------------------------------------------ エンジン (副作用)


class EngineDied(Exception):
    """repl プロセスが応答しなくなった。呼び出し側は作り直す。"""


class Interrupted(Exception):
    """評価中に Ctrl-C が来た。プロトコルがずれているので作り直す。"""


class NoEnvironment(Exception):
    """
    環境が無いのに環境の上で走らせようとした。呼ぶ側の誤り。

    boot が通らなかったあとの状態を扱い忘れると、以前は repl が Init だけの
    環境を勝手に作って答えていた (嘘の型、消える宣言)。黙って進むよりは
    ここで止める。loop が「内部エラー」として 1 行分に留めるので、セッション
    ごと落ちることはない。
    """


# 起動が駄目になる理由。どれも報告して済ませる (traceback にしない)。
START_FAILED = (EngineError, EngineDied, Interrupted, OSError)


class Undone(NamedTuple):
    """取り消した宣言 1 件。戻すのに要るものだけ。"""

    env: int | None
    src: str | None
    gen: int  # 控えたときのプロセスの世代。作り直されたら env id は死んでいる


class Loaded(NamedTuple):
    """load_file の結果。src は import を足したあとの、実際に送ったソース。"""

    resp: Response
    bad: bool
    src: str
    note: str | None = None  # init を重ね直せなかったときの理由


class Replay(NamedTuple):
    """
    replay の結果。落としたものを全部持って返す。

    件数だけ返していたら、通らなかった宣言も読み直せなかったファイルも黙って
    消えていた。環境から消えたものは必ず言う。
    """

    done: list[str]  # 通った宣言
    failed: list[str]  # 通らなかった宣言
    skipped: list[str]  # 打ち切って試していない宣言
    notes: list[str]  # init や :l で起きたこと
    sorries: list[Sorry]  # 最後に通った宣言に残った sorry

    @property
    def dropped(self) -> bool:
        """環境から消えたものがあるか。"""
        return bool(self.failed or self.skipped or self.notes)


class Engine:
    """repl サブプロセス 1 個。落ちたら restart() で作り直す。"""

    def __init__(self, cfg: EnvConfig) -> None:
        self.cfg = cfg
        self.tc = toolchain(cfg)
        self.dir = ensure_engine(cfg.engine, self.tc)
        self._warn_toolchain()
        self.proc_env = self._proc_env()
        self.proc: subprocess.Popen[str] | None = None
        self.env: int | None = None  # いまの環境 id
        self.base: int | None = None  # 起動直後 / :l 直後の環境 id
        self.stack: list[int] = []  # :undo 用
        self.log: list[str] = []  # 受理した宣言。再起動時に replay する
        self.unplayed: list[str] = []  # 打ったが env に入っていない宣言
        self.gen = 0  # プロセスの世代。proofState の持ち主の照合に使う
        self.loaded: str | None = None  # :l したファイル
        self.loaded_src: str | None = None  # その中身 (import は落とす)
        self.loaded_imports: list[str] = []  # そのファイルが書いていた import
        self.init_src: str | None = None  # init で通したソース
        self.spawn()

    # -- 環境変数 ---------------------------------------------------------

    def _warn_toolchain(self) -> None:
        """
        明示されたエンジンの版を確かめる。

        leani が用意したものは使う版でビルドしてあるので食い違わない。人が
        用意したものだけ、違っていたら言う (勝手に作り直さない)。
        """
        if self.cfg.engine is None:
            return

        engine_tc = (read_text(f"{self.dir}/lean-toolchain") or "").strip()
        if engine_tc and engine_tc != self.tc:
            print(
                yellow(
                    f"警告: toolchain が違う "
                    f"(使う版={self.tc} / エンジン={engine_tc})。\n"
                    f"  cd {self.dir} && lake build repl"
                ),
                file=sys.stderr,
            )

    def _proc_env(self) -> dict[str, str]:
        """repl に渡す環境変数。エンジンの olean を LEAN_PATH の先頭に足す。"""
        env = dict(os.environ, **lake_env(self.cfg.project, self.tc))
        env["LEAN_PATH"] = f"{self.dir}/.lake/build/lib/lean:" + env.get(
            "LEAN_PATH", ""
        )
        return env

    # -- プロセス ---------------------------------------------------------

    def spawn(self) -> None:
        # 作り直すたびに進める。前のプロセスの proofState と混ぜないため。
        self.gen += 1
        self.proc = subprocess.Popen(
            ["elan", "run", self.tc, f"{self.dir}/.lake/build/bin/repl"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=None,
            env=self.proc_env,
            text=True,
            encoding="utf-8",
            errors="replace",
            bufsize=1,
            start_new_session=True,  # Ctrl-C を自分のプロセス群だけに向ける
        )

    def kill(self) -> None:
        if self.proc and self.proc.poll() is None:
            try:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                self.proc.kill()

    # -- プロトコル -------------------------------------------------------

    def send(self, obj: Json) -> Response:
        """
        リクエストを 1 つ投げて、レスポンス 1 つを読む。

        読み取りは生の fd と select でやる。バッファ付きの readline では
        Ctrl-C がどこで効いたのか (リクエストが飛んだのか、レスポンスを
        取りこぼしたのか) が分からず、プロトコルがずれる恐れがある。
        ここで Interrupted を上げたら呼び出し側は必ずエンジンを作り直す。
        """
        try:
            return self._exchange(obj)
        except KeyboardInterrupt:
            raise Interrupted() from None

    def _exchange(self, obj: Json) -> Response:
        if self.proc is None or self.proc.poll() is not None:
            raise EngineDied()

        try:
            self.proc.stdin.write(json.dumps(obj) + "\n\n")
            self.proc.stdin.flush()
        except (BrokenPipeError, ValueError) as e:
            raise EngineDied() from e

        fd = self.proc.stdout.fileno()
        dec = codecs.getincrementaldecoder("utf-8")(errors="replace")
        buf = ""

        while True:
            try:
                ready, _, _ = select.select([fd], [], [], 0.25)
            except (OSError, ValueError) as e:
                raise EngineDied() from e

            if not ready:
                if self.proc.poll() is not None:
                    raise EngineDied()
                else:
                    continue

            try:
                chunk = os.read(fd, 1 << 16)
            except OSError as e:
                raise EngineDied() from e
            if not chunk:
                raise EngineDied()

            buf += dec.decode(chunk)
            done = first_response(buf)
            if done is not None:
                return done

    def send_cmd(self, src: str, fresh: bool = False) -> Response:
        """
        コマンドを 1 つ送る。`fresh` は「新しい環境を作る」という意思表示。

        ここが唯一の関門。`env` キーを落として送ると repl はエラーにせず、
        Init だけの環境を勝手に作って答えてしまう。だから「環境が無いときに
        どうするか」は呼ぶ側が必ず決めることにして、決めていない呼び出しは
        送る前に断る。boot が通らなかった状態を扱い忘れても、嘘の答えでは
        なく `NoEnvironment` として出る。
        """
        if fresh:
            return self.send({"cmd": src})
        elif self.env is None:
            raise NoEnvironment(f"環境が無いのに送ろうとした: {head_line(src)}")
        else:
            return self.send({"cmd": src, "env": self.env})

    def send_tactic(self, src: str, state: int) -> Response:
        return self.send({"tactic": src, "proofState": state})

    def query(self, src: str) -> str | None:
        """info メッセージの中身だけ取る。環境は進めない。"""
        if self.env is None:
            # 環境が無いなら「答えられなかった」を返す。送れば send_cmd が
            # 関門で断るが、型や補完の問い合わせは答えが無くて済む種類の
            # ものなので、例外にせず None にして呼び手に任せる。
            return None

        resp = self.send_cmd(src)
        return None if has_error(resp) else info_text(resp)

    def advance(self, resp: Response) -> None:
        if self.env is not None:
            self.stack.append(self.env)
        self.env = resp["env"]

    def pop_decl(self) -> Undone:
        """直前の宣言を環境ごと取り消す。戻せるように控えを返す。"""
        saved = Undone(self.env, self.log.pop() if self.log else None, self.gen)
        if self.stack:
            self.env = self.stack.pop()
        return saved

    def push_decl(self, saved: Undone) -> bool:
        """
        pop_decl で取り消したものを戻す。戻せなければ False。

        世代が変わっていたら戻さない。控えた env id は死んだプロセスのもので、
        新しいエンジンには無い。それを今の env に据えると、以後の cmd は
        存在しない環境に飛び (repl は "Unknown environment." を返すだけ)、
        打っても何も起きない端末になる。宣言は呼ぶ側が流し直す。
        """
        if saved.gen != self.gen:
            return False

        if saved.src is not None:
            self.log.append(saved.src)
        if saved.env is not None and self.env is not None and saved.env != self.env:
            self.stack.append(self.env)
            self.env = saved.env

        return True

    def save_header(self) -> str:
        """
        :save が書くヘッダ。設定の import に :l したファイルの import を足す。

        足さないと、:l したファイルが import していたものが落ちる。書き出しは
        「宣言 n 件を書き出した」と成功を報告するのに、そのファイルは :l でも
        lean でも通らない (Unknown identifier が並ぶ) という形で出る。
        """
        mods = ["Lean", *self.cfg.imports]
        mods += [m for m in self.loaded_imports if m not in mods]
        return "".join(f"import {m}\n" for m in mods)

    def sources(self) -> list[str]:
        """
        今の環境を作っているソース。:save がこれを書き出す。

        log だけでは足りない。init と :l したファイルは base に畳み込んで
        あるので、それも並べないと書き出したものを :l で読み直せない。

        並べる順は実際に流した順。:l は環境を作り直すので、init はその上に
        重なる (load_file が重ね直す)。逆に並べると、init が :l したファイルの
        名前を使っているときだけ書き出したファイルが通らなくなる。
        """
        parts = [self.loaded_src, self.init_src, *self.log]
        return [src.strip() for src in parts if src and src.strip()]

    # -- 起動 / 再起動 ----------------------------------------------------

    def boot(self) -> None:
        """
        設定された import を流して起点の環境を作る。

        pickle キャッシュは試したが効かないので入れていない。repl の pickle は
        import からの差分しか持たない (1.2KB 程度) ので、unpickle でも
        olean の読み込みは同じだけ走る。実測でも import 1.3s / unpickle 1.2s、
        mathlib は 5.4s / 5.3s で差が無い。さらに戻した環境で #eval すると
        Lean のコンパイラが PANIC する。セッションの保存は :save (ソース) で行う。
        """
        self.env, self.stack = None, []
        resp = self.send_cmd(self.cfg.boot_header + BOOT_PROBE, fresh=True)
        if has_error(resp):
            # ヘッダにエラーがあるか、import が 1 つでも解決できずに丸ごと
            # 捨てられたか。後者は repl が黙って env を返すので、BOOT_PROBE が
            # 通らないことでしか気付けない。そのまま起動すると import Lean も
            # 無い環境になり、完結判定も補完も宣言も全部通らなくなる。
            # 何と言われたかも出す。toolchain を差し替えて olean が食い違った
            # ときはここにしか手掛かりが無い (import 自体は書き間違っていない)。
            why = error_text(resp)
            head = f"import が通らない: {' '.join(['Lean', *self.cfg.imports])}"
            raise EngineDied(f"{head}\n{textwrap.indent(why, '  ')}" if why else head)

        self.env = self.base = resp["env"]

    def restart(self) -> Replay:
        """落ちた / 中断されたエンジンを作り直し、宣言を replay する。"""
        self.kill()
        self.spawn()

        # boot で投げたら log はそのまま残す。やり直せば replay できる。
        # 前回流せなかったぶんは env に無いので、通ったものの後ろに回す。
        log = list(self.log) + list(self.unplayed)
        loaded = self.loaded
        try:
            self.boot()
        except (EngineDied, Interrupted, OSError, KeyboardInterrupt):
            # boot が通らなかった。プロセスは建て直したので前の env id は死んで
            # いて、宣言はどこにも入っていない。log に残すと len(stack) と
            # 食い違い、:save が環境に無い宣言を本体に書く。控えに回せば
            # コメントとして添えられ、直してから :restart で流し直せる。
            # base も捨てる。:reset が死んだ id を据え直すと、submit の
            # 「env が無い」ガードが外れて何を打っても通らない端末になる。
            self.env = self.base = None
            self.stack, self.log, self.unplayed = [], [], log
            raise

        self.log, self.unplayed = [], []

        # init と :l したファイルは base に畳み込んであるので重ね直す。
        # ファイルを読み直せた場合は load_file の中で init も重なる。
        notes = []
        lost = self.reload(loaded)
        if lost:
            notes.append(lost)
        if lost or not loaded:
            note = self.reapply_init()
            if note:
                notes.append(note)

        out = self.replay(log)
        return out._replace(notes=notes + out.notes)

    def reload(self, loaded: str | None) -> str | None:
        """
        :l したファイルを読み直す。読めなかった理由を返す (None なら成功)。

        読み直せなかったのに loaded_src を残すと、env に無い宣言を sources()
        が並べ続ける。そのまま :save すると、書き出したファイルが :l で
        「すでに宣言されている」と言って通らない。
        """
        if not loaded:
            return None

        try:
            out = self.load_file(loaded)
        except OSError as e:
            why = e.strerror or str(e)
        except (EngineDied, Interrupted):
            why = "読み直している途中で止まった"
        else:
            if not out.bad:
                return out.note
            why = "読み直したら通らなかった"

        self.loaded, self.loaded_src = None, None
        return f"{loaded} を読み直せなかった: {why}"

    def reapply_init(self) -> str | None:
        """
        init を今の base に重ね直す。重ねられなければ理由を返して忘れる。

        忘れずに init_src を残すと、これも sources() が env に無い宣言を
        並べる側に回る。
        """
        if not self.init_src:
            return None

        try:
            resp = self.send_cmd(self.init_src)
        except (EngineDied, Interrupted):
            return "init を重ね直している途中で止まった"

        if has_error(resp):
            self.init_src = None
            return "init を重ね直せなかった"

        self.env = self.base = resp["env"]
        return None

    def probe_env(self, env: int) -> str | None:
        """
        その環境で import が効いているかを確かめる。効いていなければ理由を返す。

        import が 1 つでも解決できないと、repl はヘッダを丸ごと捨てて (エラーも
        出さずに) 環境を返す。boot はそれを BOOT_PROBE で見ているが、:l には
        同じ確かめが無かったので「読み込んだ」と報告してから、完結判定も補完も
        宣言も全部通らない環境に座ることになる。:save も同じ import を書く。
        """
        resp = self.send({"cmd": BOOT_PROBE, "env": env})
        if not has_error(resp):
            return None

        why = error_text(resp)
        head = "import が解決できないのでヘッダが丸ごと捨てられた"
        return f"{head}: {head_line(why)}" if why else head

    def replay(self, log: Sequence[str]) -> Replay:
        """
        宣言を今の環境に流し直す。通らなかったものは飛ばして続ける。

        :restart と :env の戻り道が同じものを使う。件数だけ返していたころは
        通らなかった宣言が黙って消えていた。
        """
        done: list[str] = []
        failed: list[str] = []
        found: list[Sorry] = []

        if self.env is None:
            # 流し直す先が無い。送れば send_cmd が関門で断つが、ここは
            # 「何件通ったか」を返す関数なので、例外を上げずに全件を控えへ
            # 回し、理由を Replay に載せて返す。
            self.unplayed = list(log) + self.unplayed
            return Replay([], [], list(log), ["環境が無いので流し直せない"], [])

        for n, src in enumerate(log):
            try:
                resp = self.send_cmd(src)
            except (EngineDied, Interrupted):
                # 試せていないものは unplayed に控える。エンジンを直せば
                # :restart でやり直せる。ここで捨てると打ったものが戻らない。
                #
                # log に混ぜてはいけない。log は env に入っている宣言の並びで、
                # stack と 1 対 1 に対応している。env に無いものを混ぜると
                # :undo と埋め戻しが別の宣言を落とす。
                rest = list(log[n:])
                self.unplayed = failed + rest + self.unplayed
                return Replay(done, failed, rest, [], found)

            if has_error(resp):
                # テキストは控えに回す。落ちたのはユーザの操作ではないので、
                # 「戻せなかった」と言うだけで打ったものを消してはいけない。
                # 読み込むファイルを直せば次の :restart で通る。
                failed.append(src)
                continue

            self.advance(resp)
            self.log.append(src)
            done.append(src)
            # 最後に通ったものだけ覚える。証明していた宣言は log の末尾に
            # 居るので、これで :prove に繋ぎ直せる。
            found = sorries(resp)

        self.unplayed = failed + self.unplayed
        return Replay(done, failed, [], [], found)

    def load_file(self, path: str, src: str | None = None) -> Loaded:
        if src is None:
            with open(path) as f:
                src = f.read()

        raw = src
        if not has_import(src):
            src = self.cfg.header + src
        src = PROBE_IMPORT + src

        keep = (self.env, self.stack, self.log, self.unplayed)
        self.env, self.stack, self.log, self.unplayed = None, [], [], []

        try:
            resp = self.send_cmd(src, fresh=True)
            why = None if has_error(resp) else self.probe_env(resp["env"])
        except (EngineDied, Interrupted, KeyboardInterrupt):
            # 中断や落ちで手元の環境と宣言を失わない。作り直せば replay できる。
            self.env, self.stack, self.log, self.unplayed = keep
            raise

        bad = has_error(resp) or why is not None
        note = why
        if not bad:
            self.env = self.base = resp["env"]
            self.loaded, self.loaded_src = path, strip_imports(raw)
            self.loaded_imports = import_lines(raw)
            # fresh で環境を作り直したので init は落ちている。重ね直さないと
            # env には無いものを sources() が並べ続ける。
            note = self.reapply_init()
        else:
            # 失敗した読み込みで手元の環境まで失わない。env が None のままだと
            # 以後の入力が import 無しの環境に飛んで、何を書いても通らなくなる。
            self.env, self.stack, self.log, self.unplayed = keep

        return Loaded(resp, bad, src, note)


# ----------------------------------------------------- 表示を組む (純粋)

# `exact?` や `simp?` は結果を "Try this:" として info で返す。台本にはこの
# 中身を入れる。`exact?` のままでは :save したファイルで毎回検索が走り、
# 結果も環境次第で変わる。提案の先頭には `[apply]` のような目印が付く。
SUGGESTION_TAG = re.compile(r"^\[[^\]]*\]\s*")

# 提案が読み切れたかの目安。折り返しを取りこぼすと必ずここが崩れる。
PAIRS = {"(": ")", "[": "]", "{": "}", "⟨": "⟩"}

PAINT: dict[str, Callable[[str], str]] = {"error": red, "warning": yellow}


def plain(s: str) -> str:
    return s


def panic_line(resp: Response) -> str | None:
    """エンジンが PANIC を吐いていたら、その 1 行目。無ければ None。"""
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
    ふつうに複数行になる。1 行目だけ取ると `simp only [a, b,` のような
    閉じていない台本になり、しかもそれが「完成した証明」として出るので
    気付けない。改行ごと返す (Lean のタクティクは複数行でよい)。
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

    位置は送ったソースの座標なので、#eval で包んだぶんの下駄 (line_off /
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


# ---------------------------------------------------- 表示を出す (副作用)


def reset_sgr() -> None:
    """libedit で開いたままにした色を戻す (pc の続き)。"""
    if TTY and RL_HOIST:
        sys.stdout.write("\033[0m")
        sys.stdout.flush()


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
        # repl がリクエストごと断った (env や proofState が無い)。messages には
        # 何も入らないので、ここで出さないと画面が無反応になる。
        print(red(note.strip()))
        print(dim("  エンジンと環境が食い違っている。:restart で建て直せる"))

    if has_error(resp):
        # 通らなかった宣言は環境に入っていない。その sorry の目標を出しても
        # 埋めようがなく、エラーの後ろに読めない目標が並ぶだけ。
        return

    for i, sy in enumerate(sorries(resp)):
        print(green(f"sorry {i + 1} [proofState {sy.get('proofState')}]"))
        print(textwrap.indent((sy.get("goal") or "").rstrip(), "  "))


# ------------------------------------------------------- フロント (副作用)

USAGE = f"""\
leani [オプション] [file.lean]

  -e, --env <name>     設定した環境で起動する
  -i, --import <Mod>   import を足す (繰り返せる)
  -p, --project <dir>  Lake プロジェクトを指定する
      --setup          エンジンを用意して終わる (普段は起動時に自動)
  -V, --version        版と置き場所
  -h, --help           これ

環境は {CONFIG} に書く。無ければ cwd の
lakefile から Lake プロジェクトと lean_lib を推測し、その外なら Lean 本体だけで
起動する。init ファイルは {INIT}。

エンジン (leanprover-community/repl) は初回だけ git clone と lake build で用意し、
使う Lean の版ごとに {ENGINE_CACHE} の下へ置く。
自分で clone したものを使うなら engine か LEANI_ENGINE で指す。

"""

HELP = """\
式を書くと #eval される。宣言はそのまま通る。入力が終わったかは Lean のパーサが決める。
続きがある行はそのまま次の行を待ち、インデントを続ける限り読む。空行で確定。
確定した直後にインデント行を書けば、直前の入力に遡って続きとして読み直す。

  :t <expr>      型 (#check)
  :i <name>      型と docstring
  :p <name>      定義 (#print)
  :l <file>      読み込む (環境を作り直す)   :r  読み直す
  :reset         起動直後に戻る              :undo [n]  n 個前の環境へ
  :env [name]    今の環境 / 設定した環境に切り替えて再起動
  :prove [n]     sorry の証明モードに入る   :goals  残っている sorry と目標
  :save <file>   通した宣言を .lean に書き出す (:l で読み直せる)
  :time          実行時間の表示を切り替え
  :{ ... :}      複数行を明示的に囲む
  :! <cmd>       shell
  :restart       エンジンを作り直して宣言を replay
  :help :?       これ                        :q  終了 (Ctrl-D)

証明モード (⊢>) では 1 行が 1 タクティク。:goals :script :undo :done
"""

PROOF_HELP = (
    "証明モード: 1 行 = 1 タクティク。:goals 目標  :script 台本  :undo 戻す  :done 出る"
)


@dataclass
class Proof:
    """証明モードの状態。1 行 = 1 タクティクで進む。"""

    state: int  # repl 側の proofState
    goals: list[str]
    script: list[str] = field(default_factory=list)
    stack: list[int] = field(default_factory=list)  # :undo 用


@dataclass(frozen=True)
class Last:
    """直前に送った入力。インデント行が来たときに遡るために覚えておく。"""

    src: str
    advanced: bool  # 環境を進めたか (取り消すべきか)
    proof: bool = False  # 証明モードでのタクティクだったか
    env_before: int | None = None


class Repl:
    """
    端末との対話。副作用の層。

    入力を 1 行受けて (feed)、完結したかをパーサに聞き (probe)、送って (submit)
    表示する (render) という流れで読める。判定と整形は上の純粋な関数に出して
    あるので、ここに残るのは状態遷移と入出力だけ。持っている状態は入力バッファ
    (buf / ready / explicit)、証明モード (proof / pending)、補完のキャッシュ。
    """

    _rl_ready = False
    _hist_ok = True  # 履歴を読めたか。読めていないなら書かない

    def __init__(self, cfg: EnvConfig, preload: str | None = None) -> None:
        self.saved: set[str] = set()  # :save で書いたもの。上書きの判断に使う
        self._start(cfg, preload)

    def _start(self, cfg: EnvConfig, preload: str | None = None) -> None:
        """エンジンを立てて起点の環境を作る。:env の切り替えでもここを通る。"""
        self.cfg = cfg

        # 入力バッファ
        self.buf: list[str] = []
        self.ready: Kind | None = None  # 構文的に完結しているときの送り方
        self.last: Last | None = None
        self.explicit = False  # :{ ... :} の中か
        self.undone: Undone | None = None  # rewind で取り消した宣言
        self.show_time = False

        # 証明モード
        self.proof: Proof | None = None
        self.proof_gen = -1  # その proof を出したエンジンの世代
        self.pending: list[Sorry] = []
        self.proof_at: Sorry | None = None  # :prove で選んだ sorry (位置つき)
        self.proof_src: str | None = None  # sorry を出した宣言のソース
        self.sorry_env: int | None = None  # その宣言が作った環境 id (照合用)

        # 補完と履歴
        self._comp_cache: dict[tuple[int | None, str], list[str]] = {}
        self._hits: list[str] = []
        self._own: tuple[int, list[str]] = (-1, [])
        self._hist_added = 0

        self.eng = Engine(cfg)
        try:
            self._setup_readline()
            t0 = time.time()
            self.eng.boot()
        except BaseException:
            # Engine を作った時点で repl は起動している。ここで投げると
            # 呼び側が self.eng を差し替えるので、殺す手立てが無くなる
            # (:env の切り替えに失敗するたび 1 プロセス残っていた)。
            self.eng.kill()
            raise
        print(dim(f"leani — {self.eng.tc} / {cfg} / {time.time() - t0:.1f}s"))

        if os.path.isfile(INIT):
            self.apply_init()
        if preload:
            self.load(preload)

    def apply_init(self) -> None:
        """
        init ファイルを起動直後の環境に重ねる。

        :l と違って環境を作り直さない。ここで通したものは base に含めるので
        :reset しても残る (GHCi の .ghci と同じ扱い)。
        """
        try:
            with open(INIT) as f:
                text = f.read()
        except OSError as e:
            print(red(f"{INIT} が読めない ({e.strerror})"))
            return

        src = strip_imports(text)
        if not src.strip():
            return

        resp = self.guard(lambda: self.eng.send_cmd(src))
        if resp is None:
            return
        elif has_error(resp):
            print(red(f"{INIT} にエラーがある:"))
            render(resp, src)
            return
        else:
            # 作り直したときに重ね直せるよう Engine に持たせる。
            self.eng.env = self.eng.base = resp["env"]
            self.eng.init_src = src
            print(dim(f"-- {INIT} を読んだ"))

    # -- readline ---------------------------------------------------------

    def _setup_readline(self) -> None:
        try:
            import readline
        except ImportError:
            self.rl = None
            return

        self.rl = readline

        global RL_OK, RL_HOIST
        RL_OK = True
        RL_HOIST = "libedit" in (readline.__doc__ or "")
        readline.set_completer(self._complete)

        if Repl._rl_ready:
            return
        Repl._rl_ready = True

        if os.path.dirname(HIST):
            with contextlib.suppress(OSError):
                os.makedirs(os.path.dirname(HIST), exist_ok=True)

        # 読めなかったファイルには書かない。write_history_file は丸ごと
        # 書き直すので、読めないまま 1 行打つと前回までの履歴が消える。
        # (makedirs と同じ suppress に入れていたので、LEANI_HISTORY に
        #  ディレクトリ成分が無いだけで毎回消えていた。)
        try:
            readline.read_history_file(HIST)
        except OSError as e:
            # libedit は中身の無いファイルでも errno を返すので、失敗した
            # ことだけでは判断できない。記録が入っていたのに読めなかった
            # ときだけ手を引く (GNU 形式の履歴がこれになる)。
            if history_entries(HIST):
                Repl._hist_ok = False
                print(
                    yellow(f"履歴が読めないので今回は書かない ({e.strerror}): {HIST}"),
                    file=sys.stderr,
                )

        readline.set_history_length(10000)
        atexit.register(self._save_history)

        # Lean の名前は . を含むので区切りにしない。
        readline.set_completer_delims(' \t\n(),[]{};"')
        if RL_HOIST:
            readline.parse_and_bind("bind ^I rl_complete")
        else:
            readline.parse_and_bind("tab: complete")

    def _save_history(self) -> None:
        """
        毎行書く。atexit だけだと落ちたセッションの履歴が丸ごと消える。
        1 万件でもミリ秒なので、書き直しのコストは問題にならない。
        """
        if not self.rl or not Repl._hist_ok:
            return
        with contextlib.suppress(OSError):
            self.rl.write_history_file(HIST)

    def _merge_history(self, src: str | None) -> None:
        """
        複数行のブロックを履歴 1 件にまとめる。

        readline は行単位なので `def fib` の 4 行は 4 件になり、呼び戻すのに
        Ctrl-P が 4 回要る。末尾の n 件がそのブロックそのものだと確認できた
        ときだけ 1 件に置き換える (数え違いで履歴を壊さないため)。
        libedit は履歴ファイル上で改行を \012 として往復できる。
        """
        n, self._hist_added = self._hist_added, 0
        if not self.rl or n < 2 or src is None:
            return

        total = self.rl.get_current_history_length()
        if total < n:
            return

        items = [self.rl.get_history_item(i) for i in range(total - n + 1, total + 1)]
        if any(x is None for x in items):
            return
        if "\n".join(items).rstrip() != src.rstrip():
            return

        for _ in range(n):
            self.rl.remove_history_item(self.rl.get_current_history_length() - 1)

        self.rl.add_history(src)
        self._save_history()  # まとめた形をファイルにも反映する

    # -- 補完 -------------------------------------------------------------

    def _complete(self, text: str, state: int) -> str | None:
        if state == 0:
            self._hits = self._names(text)
        hits = self._hits
        return hits[state] if state < len(hits) else None

    @staticmethod
    def _chunk(prefix: str) -> str:
        """
        まとめて取る単位。名前空間があればそこまで、無ければ先頭 2 文字。

        mathlib では 1 文字だと `C` で 7.5 万件 (3.4MB) になるので広げすぎない。
        `Nat.` なら 5684 件、`MeasureTheory.` なら 1 万件で収まる。
        """
        return prefix[: prefix.rfind(".") + 1] if "." in prefix else prefix[:2]

    def _names(self, prefix: str) -> list[str]:
        """定数名の prefix 検索。名前空間ごとの塊を 1 度だけ取って以後は絞る。"""
        if len(prefix) < 2:
            return []

        # 塊は base 環境 (import / :l 直後) に紐付ける。宣言を 1 つ通すたびに
        # 捨てていると mathlib では毎回 1.1 秒かかり直すので、自分で通した分だけ
        # Python 側で足す。
        key = (self.eng.base, self._chunk(prefix))
        chunk = self._comp_cache.get(key)
        if chunk is None:
            out = self.guard(
                lambda: self.eng.query(
                    COMPLETE_QUERY % (lean_str(key[1]), COMPLETE_CAP)
                )
            )
            if out is None:
                return []

            chunk = out.split()
            self._comp_cache[key] = chunk

        hits = {x for x in chunk if x.startswith(prefix)}
        hits.update(x for x in self._own_names() if x.startswith(prefix))
        return sorted(hits)

    def _own_names(self) -> list[str]:
        """REPL で通した宣言の名前。ログが伸びたときだけ数え直す。"""
        if self._own[0] != len(self.eng.log):
            names: set[str] = set()
            for src in self.eng.log:
                names.update(DECL_NAME.findall(src))
            self._own = (len(self.eng.log), sorted(names))

        return self._own[1]

    # -- エンジンの面倒を見る ---------------------------------------------

    def revive(self) -> Replay | None:
        """
        エンジンを作り直して replay する。作り直せなければ None。

        boot が通らない (import が壊れている / Ctrl-C で中断した) ことは
        ふつうに起きる。宣言は Engine 側に残るので、直してから :restart で
        やり直せる。ここで投げるとセッションごと消えるので投げない。
        """
        try:
            out = self.eng.restart()
        except Interrupted:
            print(yellow("^C 作り直しを中断した。:restart でやり直せる"))
            self.fold_proof()
            return None
        except (EngineDied, OSError) as e:
            print(red(f"エンジンを作り直せなかった: {str(e) or 'import が通らない'}"))
            print(dim("  import と設定を直してから :restart"))
            self.fold_proof()
            return None

        self.proof, self.last = None, None
        self.clear_pending()
        print(dim(f"宣言 {len(out.done)} 件を replay した (env {self.eng.env})"))
        self.report_replay(out)
        self.reattach(out)
        return out

    def fold_proof(self) -> None:
        """
        作り直しに失敗したときの後始末。環境が無いので遡る先も無い。

        成功パスだけが証明モードを畳んでいたので、失敗すると死んだ
        proofState を掴んだままになり、以降どの行も赤い "Unknown proof state."
        だけを返す幽霊の証明モードに座り続けていた (抜ける案内も出ない)。
        """
        self.last = None
        if self.proof is not None:
            self.drop_proof()
        # 持ち越した proofState も死んだプロセスのもの。残すと :goals が
        # 環境に無い宣言の目標を出し、:prove がその幽霊で証明モードに入る。
        self.clear_pending()

    def report_replay(self, out: Replay) -> None:
        """replay で落としたものを言う。黙って消えると気付く場所が無い。"""
        for note in out.notes:
            print(yellow(note))
        for src in out.failed:
            print(yellow(f"戻せなかった宣言: {head_line(src)}"))
        if out.failed or out.notes:
            print(dim("  これらは環境に無い (テキストは控えてある)"))
        if out.skipped:
            print(yellow(f"まだ流していない宣言: {len(out.skipped)} 件"))
            print(dim("  これらも環境に無い。:restart でやり直せる"))

    def reattach(self, out: Replay) -> None:
        """
        replay で戻った宣言に sorry が残っていたら :prove に繋ぎ直す。

        タクティクの途中で落ちた / Ctrl-C したとき、証明していた宣言は
        replay で戻っている。新しい proofState を拾い直さないと、宣言はある
        のに :prove が「sorry が無い」と言うだけになり、:undo しか道が無い。
        """
        if not out.sorries or not out.done:
            return

        self.pending = out.sorries
        self.proof_src = out.done[-1]
        self.sorry_env = self.eng.env
        print(dim(f"-- :prove で証明モードに入り直せる (sorry {len(out.sorries)} 個)"))

    def replay_into(self, log: Sequence[str]) -> None:
        """打った宣言を今のエンジンに流し直す。:env の戻り道で使う。"""
        if not log:
            return

        if self.eng.env is None:
            # ここは案内を出す層。関門の例外をそのまま「内部エラー」として
            # 見せると、控えが残っていることも次の一手も伝わらない。
            print(yellow(f"環境が無いので宣言 {len(log)} 件を戻せなかった"))
            print(dim("  テキストは控えてある。:restart で建て直せる"))
            self.eng.unplayed = list(log) + self.eng.unplayed
            return

        out = self.guard(lambda: self.eng.replay(log))
        if out is None:
            print(yellow(f"打った宣言 {len(log)} 件を戻せなかった"))
            return

        print(dim(f"宣言 {len(out.done)} 件を戻した (env {self.eng.env})"))
        self.report_replay(out)
        self.reattach(out)

    def guard(self, fn: Callable[[], T], on_dead: T | None = None) -> T | None:
        """
        エンジンが落ちる / 中断されたら再起動して replay する。

        落ちた場合は作り直したうえで 1 回だけやり直す。ユーザが Ctrl-C で
        止めた場合はやり直さない (止めたいのだから)。
        """
        retry = False
        try:
            return fn()
        except Interrupted:
            print(yellow("^C 中断した。エンジンを作り直す…"))
        except EngineDied:
            print(red("エンジンが落ちた。作り直す…"))
            retry = True

        if self.revive() is None:
            return on_dead

        if retry:
            try:
                return fn()
            except (Interrupted, EngineDied):
                pass

        return on_dead

    # -- 完結判定 ---------------------------------------------------------

    def parse(self, src: str) -> Json | None:
        """
        Lean のパーサに command / term / tacticSeq として読めるかを聞く。
        1 往復で 3 つとも取る。実行はしないので、ユーザ定義の notation も効く。
        """
        out = self.guard(lambda: self.eng.query(PARSE_PROBE % lean_str(src)))
        if not out:
            return None

        try:
            return json.loads(out.strip().splitlines()[-1])
        except (json.JSONDecodeError, IndexError):
            return None

    def probe(self, src: str) -> tuple[State, Kind]:
        return classify(self.parse(src))

    def probe_tac(self, src: str) -> tuple[State, Kind]:
        return classify_tac(self.parse(src))

    # -- ループ -----------------------------------------------------------

    def prompt(self) -> str:
        if self.proof is not None:
            return pc("35", "⊢> ")
        else:
            return pc("36", self.cfg.prompt)

    def read_line(self, prompt: str) -> str:
        """
        1 行読む。Ctrl-C は KeyboardInterrupt として上に返る。

        macOS の libedit は 1 文字ずつの read() の途中で SIGINT を受けると
        EINTR を握り潰して読み直すので、そのあいだの Ctrl-C は次の入力が来る
        まで効かない。打鍵の直後ミリ秒という窓なので指では届かない。
        """
        try:
            return input(prompt)
        finally:
            reset_sgr()

    def loop(self) -> int:
        while True:
            try:
                line = self.read_line(
                    pc("2", " | ") if (self.buf or self.explicit) else self.prompt()
                )
            except EOFError:
                if self.buf or self.explicit:
                    self.discard()
                    print()
                    continue
                else:
                    print()
                    return 0
            except KeyboardInterrupt:
                self.discard()
                self.last = None
                print("^C")
                continue
            except UnicodeDecodeError as e:
                # tty でない stdin (パイプ) は strict デコードになる。ここで
                # 抜けると、それまでに通した宣言ごと落ちる。その行だけ捨てる。
                print(red(f"UTF-8 として読めない行を飛ばした ({e.reason})"))
                continue

            if line.strip():
                self._hist_added += 1
            self._save_history()

            try:
                if self.feed_line(line) == "quit":
                    return 0
            except KeyboardInterrupt:
                # Engine.send の中は Interrupted に翻訳されるが、その外
                # (子プロセス・整形・補完) で来たぶんはここに落ちる。
                self.discard()
                self.last = None
                print("^C")
            except Exception as e:
                # 想定外でも 1 行分のエラーで済ませる。セッションを畳むと
                # そこまでの宣言を全部失うので、それが一番高い代償になる。
                print(red(f"内部エラー: {type(e).__name__}: {e}"))
                self.discard()

    def feed_line(self, line: str) -> Step | None:
        """端末から来た 1 件。履歴から戻ったものは改行入りで来る。"""
        # まとめた履歴を呼び戻すと改行入りの 1 行として返ってくるので、
        # 打ったときと同じ順に食わせ直す。
        lines = line.split("\n")
        for n, one in enumerate(lines):
            if n:
                self._hist_added += 1
            if self.feed(one) == "quit":
                return "quit"

        # 呼び戻した複数行はまとめて 1 件なので、末尾に空行を足して確定させる。
        waiting = bool(self.buf or self.explicit)
        if len(lines) > 1 and waiting:
            return self.feed("")
        else:
            return None

    def discard(self) -> None:
        """入力中のブロックを捨てる。Ctrl-C / Ctrl-D で呼ぶ。"""
        self.buf, self.ready, self.explicit = [], None, False
        self._hist_added = 0
        self.restore_undone()

    def feed(self, line: str) -> str | None:
        """
        1 行受け取る。"quit" を返したらループを抜ける。

        完結したかはパーサに聞くが、それだけでは足りない。Lean では
        `structure P where` や `def f := 1` はそれ自体で完結した command なので、
        パーサは「終わり」と言う。にもかかわらず次のインデント行は続きになりうる。
        そこで二段構えにする:

        * パーサが「途中」と言えば継続行を読む (ブロックに入る)。
        * ブロック中は、インデント行が続く限り読む。空行かインデントの切れた行で確定。
        * ブロックに入らず確定したあとにインデント行が来たら、直前の入力に
          遡って続きとして読み直す (環境も 1 つ戻す)。
        """
        if self.explicit:
            step = self.feed_explicit(line)
        elif not self.buf:
            step = self.feed_first(line)
        else:
            step = self.feed_more(line)

        if step != "probe":
            return "quit" if step == "quit" else None

        src = "\n".join(self.buf)
        proving = self.proof is not None
        state, kind = self.probe_tac(src) if proving else self.probe(src)
        if proving and self.proof is None:
            # プローブ自体が guard 経由でエンジンの死を踏み、revive が証明モードを
            # 畳んだ。このまま submit すると self.proof を見ないので command 経路に
            # 落ち、タクティクの行が宣言として送られて
            # "unexpected identifier; expected command" になる。しかも
            # submit_cmd の clear_pending が reattach の成果を消すので、直前に出した
            # 「:prove で入り直せる」まで嘘になる。行は捨てて案内だけ残す。
            self.buf, self.ready = [], None
            print(dim("  打っていた行は送らなかった"))
            return None
        elif state == MORE:
            self.ready = None
            return None
        elif block_continues(self.buf, src):
            self.ready = kind  # 完結。ただし続きがありうるので空行を待つ
            return None
        else:
            self.buf, self.ready = [], None
            self.submit(src, kind)
            return None

    def feed_explicit(self, line: str) -> Step:
        """:{ ... :} の中。:} が来るまで何も判定せずに溜める。"""
        if line.strip() == ":}":
            src, self.buf, self.explicit = "\n".join(self.buf), [], False
            if src.strip():
                self.submit(src)
        else:
            self.buf.append(line)
        return "done"

    def feed_first(self, line: str) -> Step:
        """バッファが空のときの 1 行目。"""
        s = line.strip()
        if not s:
            self.last = None
            return "done"
        elif s == ":{":
            self.explicit, self.buf = True, []
            return "done"
        elif s.startswith(":"):
            self.last = None
            self._hist_added = 0
            return "quit" if self.meta(s) == "quit" else "done"
        elif continues(line) and self.last is not None:
            # 確定した入力の続きだった。1 つ戻して書き直す。
            self.rewind()
            self.buf = self.last.src.splitlines() + [line]
            self.last = None
            return "probe"
        else:
            self.buf = [line]
            return "probe"

    def feed_more(self, line: str) -> Step:
        """ブロックの 2 行目以降。"""
        if not line.strip():  # 空行で確定
            src, kind = "\n".join(self.buf).rstrip(), self.ready
            self.buf, self.ready = [], None
            if src.strip():
                self.submit(src, kind)
            return "done"
        elif META_LINE.match(line):  # ブロックからの脱出
            src, kind = "\n".join(self.buf), self.ready
            self.buf, self.ready = [], None
            if kind is not None:
                self.submit(src, kind)
            else:
                self.restore_undone()
                print(dim("-- 未完のまま破棄した"))
                self.last = None
            return "quit" if self.feed(line) == "quit" else "done"
        elif self.ready is not None and not continues(line):
            # 確定済みのブロックにインデントの切れた行 → ここで切って読み直す
            src, kind = "\n".join(self.buf), self.ready
            self.buf, self.ready = [], None
            self.submit(src, kind)
            return "quit" if self.feed(line) == "quit" else "done"
        else:
            self.buf.append(line)
            return "probe"

    def rewind(self) -> None:
        """直前の入力を取り消して、続きを書けるようにする。"""
        last = self.last
        if last is None or not last.advanced:
            return
        elif last.proof:
            # 証明モードのタクティクは環境を進めていない。畳まれていても
            # ここで宣言を pop してはいけない (直前の本物の宣言が消える)。
            if self.proof is None:
                return
            if self.proof.stack:
                self.proof.state = self.proof.stack.pop()
            if self.proof.script:
                self.proof.script.pop()
            return
        else:
            # 書き直しをやめたときに戻せるよう控える (C-c / C-d / ブロック脱出)。
            self.undone = self.eng.pop_decl()

    def restore_undone(self) -> None:
        """rewind で取り消した宣言を戻す。書き直さずにやめたとき。"""
        if self.undone is None:
            return

        undone, self.undone = self.undone, None
        if not self.eng.push_decl(undone) and undone.src is not None:
            # 控えている間にエンジンが建て直された。控えた env id は死んで
            # いるので据えられない。テキストから流し直す。
            self.replay_into([undone.src])

    # -- 送信 -------------------------------------------------------------

    def submit(self, src: str, kind: Kind | None = None) -> None:
        """完結した入力を送って結果を出す。"""
        if self.eng.env is None:
            # boot が通らなかったエンジン。送れば send_cmd が関門で断るが、
            # 打った本人に要るのは例外の名前ではなく次の一手なので、ここで
            # 案内に変える。
            print(red("エンジンが使えない。:restart で建て直す"))
            return

        self.undone = None  # 書き直しが確定した。もう戻さない
        self._merge_history(src)
        if self.proof is not None:
            self.tactic(src)
            return

        if kind is None:
            _, kind = self.probe(src)

        t0 = time.time()
        self.last = None
        sent = self.submit_term(src) if kind == TERM else self.submit_cmd(src)
        if sent:
            self.timing(t0)

    def submit_term(self, src: str) -> bool:
        """式として #eval に包んで送る。環境は進めない。"""
        wrapped = "#eval\n" + textwrap.indent(src, "  ")
        resp = self.guard(lambda: self.eng.send_cmd(wrapped))
        if resp is None or panic_check(resp):
            return False

        errs = errors(resp)
        blob = "\n".join(m.get("data", "") for m in errs)

        # `do` を単体で書くと最初の action からモナドが決まってしまう
        # (IO.getEnv なら BaseIO)。GHCi と同じく IO と読み直してやる。
        if errs and src.lstrip().startswith("do") and "BaseIO" in blob:
            retry = "#eval show IO _ from\n" + textwrap.indent(src, "  ")
            again = self.guard(lambda: self.eng.send_cmd(retry))
            if again is not None and not has_error(again):
                render(again, src, line_off=1, col_off=2)
                self.last = Last(src, advanced=False)
                return True

        # 評価できない式でも、型だけは出したほうが親切。
        if errs and NOT_EVALUABLE.search(blob):
            out = self.guard(
                lambda: self.eng.query("#check\n" + textwrap.indent(src, "  "))
            )
            if out:
                print(out.rstrip())
                print(dim("-- 評価できないので型だけ"))
                self.last = Last(src, advanced=False)
                return True

        render(resp, src, line_off=1, col_off=2)
        self.last = Last(src, advanced=False)
        return True

    def submit_cmd(self, src: str) -> bool:
        """command としてそのまま送る。通れば環境が 1 つ進む。"""
        env_before = self.eng.env
        resp = self.guard(lambda: self.eng.send_cmd(src))
        if resp is None or panic_check(resp):
            return False

        advanced = not has_error(resp) and "env" in resp
        if advanced:
            self.eng.advance(resp)
            self.eng.log.append(src)
        render(resp, src)

        found = sorries(resp)
        self.clear_pending()
        if found and advanced:
            # 通らなかった宣言の sorry は持ち越さない。埋め戻しても同じ
            # エラーで弾かれるだけで、埋め戻しに失敗した直後に「sorry 1 個」
            # と出してから「sorry 2 個」と言い直すことになる。
            self.pending = found
            # env id を覚えておく。:undo などで環境が動いたら埋め戻しでは
            # 巻き戻さない (二重に pop して手前の宣言を落とすため)。
            self.proof_src = src
            self.sorry_env = self.eng.env
            print(dim(f"-- :prove で証明モードに入る (sorry {len(found)} 個)"))

        self.last = Last(src, advanced=advanced, env_before=env_before)
        return True

    def clear_pending(self) -> None:
        """sorry まわりの持ち越しを捨てる。環境が動いたら proofState は無効。"""
        self.pending = []
        self.proof_at = None
        self.proof_src, self.sorry_env = None, None

    def timing(self, t0: float) -> None:
        if self.show_time:
            print(dim(f"({time.time() - t0:.2f}s)"))

    # -- 証明モード -------------------------------------------------------

    def tactic(self, src: str) -> None:
        """1 行 = 1 タクティク。proofState を進める。"""
        proof = self.proof
        if proof is None:
            return

        before = proof.state
        gen = self.eng.gen
        resp = self.guard(lambda: self.eng.send_tactic(src, before))
        if self.proof is not proof or self.eng.gen != gen:
            # guard がエンジンを作り直した。手元の proofState は前のプロセスの
            # ものなので、返事が来ていても中身が違う。新しいエンジンは番号を
            # 0 から振り直すので、他の証明の状態に当たって「証明完了」まで
            # 出てしまう (宣言は sorry のまま残る)。
            self.drop_proof()
            return
        elif resp is None:
            return

        self.last = Last(src, advanced=False, proof=True)

        if resp.get("message"):  # エンジンからの素のエラー
            print(red(resp["message"].rstrip()))
            return
        elif has_error(resp):
            render(resp, src)
            return

        for m in messages(resp):
            if m.get("data"):
                print(m["data"].rstrip())

        if "proofState" not in resp:
            render(resp, src)
            return

        proof.stack.append(before)
        proof.state = resp["proofState"]

        found = try_this(messages(resp))
        if found and not balanced(found):
            # 提案を読み切れていない (メッセージの形が変わった等)。壊れた
            # 台本を完成した証明として出すより、打った通りを残す。
            print(dim("-- 提案を読み切れなかったので打った通りを台本に入れた"))
            found = None
        if found and found != src:
            shown = found.replace("\n", " ")
            print(dim(f"-- 台本には {shown} を入れた"))
        proof.script.append(found or src)
        self.last = Last(src, advanced=True, proof=True)

        goals = resp.get("goals") or []
        if goals:
            self.show_goals(goals)
            return

        script = "\n".join(proof.script)
        print(green("証明完了。"))
        print(dim("-- 台本:"))
        print(textwrap.indent(script, "  "))
        self.proof, self.last = None, None
        self.close_sorry(script)

    def drop_proof(self) -> None:
        """エンジンが作り直されたので証明モードを畳む。何が起きたかは言う。"""
        self.proof, self.last = None, None
        print(yellow("エンジンが変わったので証明モードを抜けた"))
        print(dim("  打っていたタクティクは通っていない"))
        if self.pending and self.eng.env is not None:
            # revive が replay で拾い直していれば、そのまま入り直せる。
            print(dim(f"  :prove で入り直せる (sorry {len(self.pending)} 個)"))
        else:
            # 作り直せなかったときの proofState は前のプロセスのもの。
            self.clear_pending()
            print(dim("  :restart で建て直してから打ち直す"))

    def close_sorry(self, script: str) -> None:
        """`by sorry` を台本で埋め戻して、宣言を本物として通し直す。"""
        src, at = self.proof_src, self.proof_at
        if not src or at is None:
            return

        new_src = splice_sorry(src, at, script)
        if new_src is None:
            # 位置が読めなかった。証明そのものは通っているので台本は上に
            # 出ている。黙って戻ると「証明完了」だけが残る。
            print(dim("-- 位置が読めなかったので宣言は sorry のまま"))
            return

        # sorry 版と同じ名前になるので、先に取り消してから通し直す。
        undone = None
        if self.sorry_env is not None and self.eng.env == self.sorry_env:
            undone = self.eng.pop_decl()

        # 通し直しに失敗したら戻せるよう控える。sorry が 2 個以上あるときに
        # 持ち越しを捨てると、残りを :prove で続けられなくなる。
        keep = (self.pending, self.proof_at, self.proof_src, self.sorry_env)
        self.proof_at, self.proof_src, self.sorry_env = None, None, None

        print(dim("-- 埋め戻して通す:"))
        print(textwrap.indent(new_src.strip(), "  "))

        gen = self.eng.gen
        self.submit(new_src, CMD)
        landed = bool(self.eng.log) and self.eng.log[-1] == new_src
        if undone is None or landed:
            # 着地したかは env の中身で決める。世代だけを見ると、落ちた
            # エンジンを guard が建て直して再送し**通った**ときにも
            # 「sorry のまま」と嘘をつき、sorry 版を流し直して重複エラーの
            # 宣言が控えに永久に居座る。
            return
        elif self.eng.gen != gen:
            # 建て直されて、そのうえ通らなかった。控えた env id は死んで
            # いるので据えず、sorry 版のテキストを流し直す。持ち越しは
            # 戻さない (死んだ proofState で replay_into が付け直した値を
            # 上書きすると、次の :prove が今の環境に無い状態を指す)。
            print(dim("-- エンジンが建て直されたので sorry のままにしておく"))
            if undone.src is not None:
                self.replay_into([undone.src])
        else:
            # 通らなかった。項の位置の sorry ではタクティクを差せない。
            self.eng.push_decl(undone)
            self.pending, self.proof_at, self.proof_src, self.sorry_env = keep
            print(dim("-- 通らなかったので sorry のままにしておく"))
            left = len(self.pending)
            if left > 1:
                print(dim(f"  残りは :prove <n> で続けられる (sorry {left} 個)"))

    def show_goals(self, goals: Sequence[str] | None = None) -> None:
        if goals is None:
            goals = self.proof.goals if self.proof else []
        if self.proof is not None:
            self.proof.goals = list(goals)

        for n, g in enumerate(goals):
            head = f"goal {n + 1}/{len(goals)}" if len(goals) > 1 else "goal"
            print(green(head))
            print(textwrap.indent(g, "  "))

    def show_pending(self) -> None:
        """証明モードの外での :goals。残っている sorry を出す。"""
        if not self.pending:
            print(dim("証明モードではない (sorry も残っていない)"))
            return

        for n, sy in enumerate(self.pending):
            print(green(f"sorry {n + 1} [proofState {sy.get('proofState')}]"))
            print(textwrap.indent((sy.get("goal") or "").rstrip(), "  "))

    def prove(self, arg: str) -> None:
        """直前の入力に出た sorry を 1 つ選んで証明モードに入る。"""
        found = self.pending
        if not found:
            print(red("直前の入力に sorry が無い"))
            return

        # isdigit は '²' に True を返すが int() は通らない。
        n = int(arg) - 1 if arg.isdecimal() else 0
        if not 0 <= n < len(found):
            print(red(f"sorry は {len(found)} 個。1..{len(found)} で指定する"))
            return

        sy = found[n]
        goal = sy.get("goal", "")
        self.proof = Proof(state=sy["proofState"], goals=[goal])
        self.proof_gen = self.eng.gen
        self.proof_at = sy  # 埋め戻すのはこの sorry。位置で切る
        print(dim(PROOF_HELP))
        self.show_goals([goal])

    # -- メタコマンド -----------------------------------------------------

    def meta(self, line: str) -> str | None:
        """`:` で始まる行を捌く。"quit" を返したらループを抜ける。"""
        if line.startswith(":!"):
            try:
                subprocess.run(line[2:].strip(), shell=True, check=False)
            except KeyboardInterrupt:
                # 子は同じプロセスグループにいるので Ctrl-C はこちらにも来る。
                # 止めたいのは子だけ。ここで抜けると宣言を全部失う。
                print("^C")
            return None

        parts = line.split(None, 1)
        cmd = parts[0][1:]
        arg = parts[1].strip() if len(parts) > 1 else ""

        # 証明モード専用のものを先に見る。扱われなければ通常のコマンドとして続ける
        # (:t などは証明中でも使える)。
        if self.proof is not None and self.proof_meta(self.proof, cmd):
            return None

        match cmd:
            case "q" | "quit":
                return "quit"
            case "?" | "h" | "help":
                print(HELP, end="")
            case "goals" | "g":
                self.show_pending()
            case "script" | "done":
                print(dim("証明モードではない"))
            case "t" | "type":
                self.cmd_type(arg)
            case "i" | "info":
                self.cmd_info(arg)
            case "p" | "print":
                self.cmd_print(arg)
            case "l" | "load":
                self.load(arg)
            case "r" | "reload":
                if self.eng.loaded:
                    self.load(self.eng.loaded)
                else:
                    self.reset()
            case "reset":
                self.reset()
            case "undo":
                self.cmd_undo(arg)
            case "env":
                self.cmd_env(arg)
            case "prove":
                self.prove(arg)
            case "save":
                self.cmd_save(arg)
            case "time":
                self.show_time = not self.show_time
                print(dim(f"実行時間の表示: {'on' if self.show_time else 'off'}"))
            case "restart":
                self.cmd_restart()
            case _:
                print(red(f"不明なコマンド: :{cmd}  (:help)"))

        return None

    def proof_meta(self, proof: Proof, cmd: str) -> bool:
        """証明モード専用のコマンド。扱ったら True。"""
        match cmd:
            case "goals" | "g":
                self.show_goals()
            case "script":
                print(textwrap.indent("\n".join(proof.script) or "(空)", "  "))
            case "undo":
                self.proof_undo(proof)
            case "done" | "q" | "quit":
                self.proof, self.buf, self.ready = None, [], None
                print(dim("証明モードを出た"))
            case "?" | "h" | "help":
                print(PROOF_HELP)
            case _:
                return False

        return True

    def proof_undo(self, proof: Proof) -> None:
        if not proof.stack:
            print(dim("戻る先が無い"))
            return

        proof.state = proof.stack.pop()
        if proof.script:
            proof.script.pop()

        print(dim(f"proofState {proof.state}"))

    def cmd_type(self, arg: str) -> None:
        if not arg:
            print(red(":t には式が要る"))
            return

        out = self.guard(
            lambda: self.eng.query("#check\n" + textwrap.indent(arg, "  "))
        )
        print(out.rstrip() if out else red("型が取れなかった"))

    def cmd_info(self, arg: str) -> None:
        if not arg:
            print(red(":i には名前が要る"))
            return

        out = self.guard(lambda: self.eng.query(f"#check @{arg}"))
        print(out.rstrip() if out else red(f"不明: {arg}"))

        doc = self.guard(lambda: self.eng.query(DOC_QUERY % arg))
        if doc and doc.strip():
            print(dim(doc.strip()))

    def cmd_print(self, arg: str) -> None:
        if not arg:
            print(red(":p には名前が要る"))
            return

        resp = self.guard(lambda: self.eng.send_cmd(f"#print {arg}"))
        if resp is not None:
            render(resp, arg)

    def cmd_undo(self, arg: str) -> None:
        for _ in range(int(arg) if arg.isdecimal() else 1):
            if self.eng.stack:
                self.eng.env = self.eng.stack.pop()
            if self.eng.log:
                self.eng.log.pop()

        self.clear_pending()
        print(dim(f"env {self.eng.env}"))

    def cmd_restart(self) -> None:
        self.revive()

    def cmd_env(self, arg: str) -> None:
        """引数なしで今の環境、名前を渡すとその環境で起動し直す。"""
        if not arg:
            self.show_env()
            return
        elif arg == self.cfg.name:
            print(dim(f"すでに {arg}"))
            return

        try:
            target = resolve(name=arg)
        except ConfigError as e:
            print(red(str(e)))
            return

        try:
            why = problem(target)
            if why is None:
                prepare(target)  # 落とす前に用意まで済ませる
        except EngineError as e:
            why = str(e)

        if why:
            print(red(why))  # 今のエンジンは落とさない
            return

        keep, prev, back = self.show_time, self.eng.loaded, self.cfg
        # 打った宣言も控える。:l した中身は preload で戻るが、対話で打った
        # ぶんは新しい Engine には入っていない。戻り道で流し直す。
        # 環境に無い宣言も連れて行く。捨てると、直前に「テキストは控えてある」
        # と言ったものが :env で黙って消える。新しい環境なら通ることもある
        # (import が増える方向の切り替え)。通らなければまた控えに戻る。
        log = list(self.eng.log) + list(self.eng.unplayed)
        self.eng.kill()
        try:
            self._start(target, preload=prev)
        except START_FAILED as e:
            # import が通るかは boot するまで分からない。元の環境に戻す。
            print(red(f"{arg} で起動できなかった: {str(e) or 'import が通らない'}"))
            print(dim(f"  {back.name} に戻る"))
            try:
                self._start(back, preload=prev)
            except START_FAILED as back_e:
                die(f"{back.name} にも戻れなくなった: {back_e}")

        # 成功しても元に戻っても、対話で打った宣言は新しいエンジンには無い。
        # except の中だけで流していたので、切り替えが成功したときに限って
        # 打った宣言が黙って消えていた。
        self.replay_into(log)
        self.show_time = keep

    def show_env(self) -> None:
        print(dim(f"{self.cfg} — {self.cfg.project or 'Lake プロジェクト無し'}"))
        print(
            dim(
                f"env {self.eng.env} "
                f"(base {self.eng.base}, 宣言 {len(self.eng.log)} 件)"
            )
        )
        try:
            names = sorted(load_config().get("env") or {})
        except ConfigError as e:
            print(red(str(e)))
            return

        if names:
            print(dim(f"切り替え先: {' '.join(names)}  (:env <name>)"))

    def cmd_save(self, arg: str) -> None:
        """
        通した宣言を .lean として書き出す。

        repl には環境を pickle する機能もあるが、戻した環境で #eval すると
        Lean のコンパイラが PANIC する (コンパイラの状態が pickle に入らない)。
        ソースで持っておけば編集もできるし lean でそのまま走る。
        """
        if not arg:
            print(red(":save にはファイル名が要る"))
            return

        srcs = self.eng.sources()
        orphans = list(self.eng.unplayed)
        if not srcs and not orphans:
            print(red("保存する宣言が無い"))
            return

        path = os.path.abspath(os.path.expanduser(arg))
        if os.path.exists(path) and path not in self.saved:
            # 打ち間違いでプロジェクトのソースを潰さない。2 度目からは上書きする。
            print(red(f"すでにある: {path}"))
            print(dim("  消すか別の名前にする"))
            return

        parts = list(srcs)
        if orphans:
            # 環境に入らなかった宣言はコメントとして添える。落とすと :save は
            # 成功を報告したのに打ったものが消える。そのまま書けば lean で
            # 通らないファイルになる。
            note = "\n\n".join(textwrap.indent(one, "-- ") for one in orphans)
            parts.append(f"-- 環境に入らなかった宣言 ({len(orphans)} 件):\n{note}")

        body = "\n\n".join(parts)
        try:
            with open(path, "w") as f:
                # 起動と同じヘッダを書く。設定の import だけだと lean で
                # 直接通らない (leani は :l のときだけ import Lean を足す)。
                # :l したファイルの import も足す (save_header)。
                f.write(f"{self.eng.save_header()}\n{body}\n")
        except OSError as e:
            print(red(f"書き出せなかった: {e}"))
            return

        self.saved.add(path)
        print(dim(f"宣言 {len(srcs)} 件を書き出した: {path}"))
        print(dim(f"-- :l {path}"))

    def load(self, path: str, announce: bool = True) -> None:
        if not path:
            print(red("ファイル名が要る"))
            return

        path = os.path.expanduser(path)
        if not os.path.isfile(path):
            print(red(f"ファイルが無い: {path}"))
            return

        try:
            with open(path) as f:
                src = f.read()
        except OSError as e:
            print(red(f"読めない: {path} ({e.strerror})"))
            return

        left = len(self.eng.unplayed)  # 読み込みが通れば控えも作り直される
        out = self.guard(lambda: self.eng.load_file(path, src))
        if out is None:
            return

        render(out.resp, out.src)
        if out.note:
            print(yellow(out.note))
        if out.bad:
            print(red(f"読み込めなかった: {path}"))
        else:
            # 環境が総取り替えになるので、前の環境の proofState は使えない。
            self.proof, self.last, self.undone = None, None, None
            self.clear_pending()
            if announce:
                print(dim(f"読み込んだ: {path} (env {self.eng.env})"))
            if left:
                # :reset と同じで、黙って捨てると気付く場所が無い。
                print(dim(f"  環境に無かった宣言 {left} 件も捨てた"))

        self._comp_cache.clear()

    def reset(self) -> None:
        if self.eng.base is None:
            # 起点の環境が無い (boot が通らなかった)。据え直しても
            # "Unknown environment." しか返さない端末になり、控えだけが消える。
            print(red("起点の環境が無い。:restart で建て直す"))
            return

        self.eng.env, self.eng.stack, self.eng.log = self.eng.base, [], []
        # 控えも捨てる。残すと、:reset で消したはずの宣言が次の :restart で
        # 戻ってくる。
        left, self.eng.unplayed = len(self.eng.unplayed), []
        self.proof, self.last = None, None
        self.clear_pending()
        print(dim(f"env {self.eng.env} に戻した"))
        if left:
            print(dim(f"  環境に無かった宣言 {left} 件も捨てた"))


@dataclass
class Args:
    """CLI 引数を読んだ結果。"""

    name: str | None = None  # -e
    project: str | None = None  # -p
    imports: list[str] = field(default_factory=list)  # -i (繰り返せる)
    preload: str | None = None  # 起動時に読み込むファイル
    setup: bool = False  # --setup
    version: bool = False
    help: bool = False


def parse_args(argv: Sequence[str]) -> Args:
    """引数を読む。値の無いオプションはその場で断る。"""
    args = Args()
    rest = list(argv)

    def value(flag: str, what: str) -> str:
        if not rest:
            die(f"{flag} には{what}が要る")
        return rest.pop(0)

    while rest:
        a = rest.pop(0)
        match a:
            case "-e" | "--env":
                args.name = value(a, "名前")
            case "-i" | "--import":
                args.imports.append(value(a, "モジュール名"))
            case "-p" | "--project":
                args.project = value(a, "ディレクトリ")
            case "--setup":
                args.setup = True
            case "-h" | "--help":
                args.help = True
            case "-V" | "--version":
                args.version = True
            case _ if a.startswith("-") and a != "-":
                die(f"不明なオプション: {a}  (-h で使い方)")
            case _:
                args.preload = a

    return args


def print_version(cfg: EnvConfig) -> None:
    tc = guess_toolchain(cfg)
    engine = engine_dir(cfg.engine, tc) if tc else f"{ENGINE_CACHE}/<版>"
    ready = "" if os.path.isfile(f"{engine}/.lake/build/bin/repl") else " (未ビルド)"

    print("leani")
    print(f"  環境:     {cfg}")
    print(f"  プロジェクト: {cfg.project or '(無し)'}")
    print(f"  Lean:     {tc or '(不明)'}")
    print(f"  エンジン: {engine}{ready}")
    print(f"  設定:     {CONFIG}{'' if os.path.isfile(CONFIG) else ' (無し)'}")
    print(f"  init:     {INIT}")
    print(f"  履歴:     {HIST}")


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(list(sys.argv if argv is None else argv)[1:])
    if args.help:
        print(USAGE + HELP, end="")
        return 0

    # 端末でない stdin は strict デコードになり、壊れたバイト 1 つで
    # UnicodeDecodeError になる。しかも投げた時点で読み込み済みのぶんが
    # 一緒に落ちるので、後続の行まで消える。置き換えて Lean に渡し、
    # 構文エラーとして普通に報告させる。
    if not sys.stdin.isatty():
        with contextlib.suppress(OSError, ValueError, AttributeError):
            sys.stdin.reconfigure(errors="replace")

    try:
        cfg = resolve(args.name, args.project, args.imports)
    except ConfigError as e:
        die(str(e))

    if args.version:
        print_version(cfg)
        return 0

    why = problem(cfg)
    if why:
        die(why)

    try:
        if args.setup:
            tc = toolchain(cfg)
            path = engine_dir(cfg.engine, tc)
            if os.path.isfile(f"{path}/.lake/build/bin/repl"):
                print(dim(f"用意済み: {path}"))
                return 0

            ensure_engine(cfg.engine, tc, asked=True)
            return 0

        # loop() も中に入れる。再起動でエンジンを用意し直せないことがある。
        return Repl(cfg, args.preload).loop()
    except START_FAILED as e:
        die(str(e) or "エンジンが起動しなかった (import が通らない)")
    except KeyboardInterrupt:
        print()
        return 130


if __name__ == "__main__":
    sys.exit(main())
