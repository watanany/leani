"""エントリーポイント (副作用)。

引数を読んで Repl を起動する。"""

from __future__ import annotations

import contextlib
import io
import os
import sys
from collections.abc import Sequence
from dataclasses import dataclass, field

from leani.boot import ensure_engine, guess_toolchain, toolchain
from leani.config import EnvConfig, problem, resolve
from leani.places import CONFIG, ENGINE_CACHE, HIST, INIT
from leani.pure import dim, engine_dir
from leani.repl import HELP, Repl
from leani.show import die
from leani.types import START_FAILED, ConfigError

USAGE = f"""\
leani [オプション] [file.lean]

  -e, --env <name>     設定した環境で起動する
  -i, --import <Mod>   import するモジュールを追加する (繰り返せる)
  -p, --project <dir>  Lake プロジェクトを指定する
      --setup          エンジンを用意して終了する (通常は起動時に自動で用意する)
  -V, --version        環境、Lean のバージョン、ファイルの場所を表示する
  -h, --help           このヘルプを表示する

環境は {CONFIG} に書く。設定が無ければ、カレントディレクトリの
lakefile から Lake プロジェクトと lean_lib を推測する。Lake プロジェクトの外では
Lean 本体だけで起動する。init ファイルは {INIT}。

エンジン (leanprover-community/repl) は初回だけ git clone と lake build で用意し、
使う Lean のバージョンごとに {ENGINE_CACHE} の下に置く。
自分でビルドしたエンジンを使う場合は、設定の engine か LEANI_ENGINE で指定する。

"""


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
    """引数を読む。値の無いオプションはその場でエラーにする。"""
    args = Args()
    rest = list(argv)

    def value(flag: str, what: str) -> str:
        if not rest:
            die(f"{flag} には{what}が必要")
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
    engine = engine_dir(cfg.engine, tc) if tc else f"{ENGINE_CACHE}/<バージョン>"
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

    # stdin が端末でないときは strict でデコードされ、不正なバイトが 1 つあるだけで
    # UnicodeDecodeError になる。しかも例外が発生した時点で読み込み済みのデータも
    # 失われるので、後続の行まで消える。不正なバイトを置換文字に置き換えて Lean に
    # 渡し、構文エラーとして普通に報告させる。
    if not sys.stdin.isatty() and isinstance(sys.stdin, io.TextIOWrapper):
        with contextlib.suppress(OSError, ValueError):
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

        # loop() も try の中で呼ぶ。エンジンの再起動時に、エンジンを用意し直せない
        # ことがある。
        return Repl(cfg, args.preload).loop()
    except START_FAILED as e:
        die(str(e) or "エンジンが起動しなかった")
    except KeyboardInterrupt:
        print()
        return 130


if __name__ == "__main__":
    sys.exit(main())
