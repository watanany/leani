"""設定 (読み取り)。

どの Lake プロジェクトの上で何を import して起動するか。ファイルと環境変数を
見るが、何も書き換えない。"""

from __future__ import annotations

import os
import re
import shutil
import tomllib
from collections.abc import Sequence
from dataclasses import dataclass
from typing import Any

from leani.places import CONFIG, ENGINE, ENGINE_REPO, PROBE_IMPORT, PROMPT
from leani.types import ConfigError, Json


@dataclass(frozen=True)
class EnvConfig:
    """
    どの Lake プロジェクトの上で何を import して起動するか。

    ghci の ~/.ghci、ipython の profile と同じ位置づけ。特定のプロジェクト名を
    コードに書かないため、名前は設定ファイル (config.toml) と CLI 引数だけに書く。
    設定が無くても、leani はカレントディレクトリの Lake プロジェクトから推測して
    起動する。

    作ったあとは変えないので frozen にする。作るときは値を整える make を使う。
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
    """設定の文字列 1 つ。型が違えばエラーにする (traceback は出さない)。"""
    if value is None or isinstance(value, str):
        return value
    else:
        raise ConfigError(
            f"{where} は文字列で書く (今は {type(value).__name__}): {CONFIG}"
        )


def as_imports(value: Any, where: str) -> Sequence[str]:
    """
    import のリスト。

    文字列 1 つをエラーにせずに受け取ると 1 文字ずつの import になる。どれも解決
    できないので、repl はヘッダ全体を捨てる (import Lean も消える)。なのでエラーにする。
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
    """設定の表 1 つ。型が違えばエラーにする。"""
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
        raise ConfigError(f"設定ファイルを読めない: {path}\n  {e}") from e


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

    return re.findall(r"^\s*lean_lib\s+«?([A-Za-z0-9_.\']+)»?", text, re.MULTILINE)


def problem(cfg: EnvConfig) -> str | None:
    """
    この環境で起動できない理由。無ければ None。

    起動時と :env の切り替え時の両方で使う。切り替えでは今のエンジンを
    終了させる前に呼ぶので、問題があれば今のセッションを残したままエラーにできる。
    この関数は状態を見るだけで、何も変えない。
    """
    # Lean のバージョンが違うエンジンは `.olean` を読めず、repl は起動直後に
    # 異常終了する。
    proj_tc = read_toolchain(cfg.project)
    eng_tc = read_toolchain(cfg.engine) if cfg.engine else None

    reasons = [
        (
            bool(cfg.project) and not os.path.isdir(str(cfg.project)),
            f"Lake プロジェクトが無い: {cfg.project}",
        ),
        (not shutil.which("elan"), "elan が PATH に無い"),
        (not shutil.which("lake"), "lake が PATH に無い"),
        # leani がエンジンを用意する場合に必要なもの。取得する前に確認する。
        (
            cfg.engine is None and not shutil.which("git"),
            f"git が PATH に無い。エンジン ({ENGINE_REPO}) の取得に使う",
        ),
        # 指定されたエンジンは leani が管理しないので、必要なファイルがあるかだけ
        # 確認する。
        (
            cfg.engine is not None and not os.path.isdir(cfg.engine),
            f"指定されたエンジンが無い: {cfg.engine}",
        ),
        # エンジンの Lean のバージョンを決めるのはこのファイルだけ。無いと elan の
        # デフォルトの toolchain が使われ、エラーが出ないまま正しく実行できなくなる。
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
        # 起動しても異常終了するだけなので、警告ではなくエラーにする。
        (
            eng_tc is not None and proj_tc is not None and eng_tc != proj_tc,
            (
                f"エンジンのバージョンがプロジェクトと違う "
                f"(プロジェクト={proj_tc} / エンジン={eng_tc})。\n"
                f"  {proj_tc} でビルドし直す: cd {cfg.engine} && "
                f"echo {proj_tc} > lean-toolchain && lake build repl\n"
                "  または engine の指定 (env.<名前>.engine、engine、LEANI_ENGINE) "
                "を外して leani に用意させる"
            ),
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
    """名前で選んだ環境。設定に書いてある通りに使う (カレントディレクトリは見ない)。"""
    if name not in table:
        known = " ".join(sorted(table)) or "(1 つも無い)"
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
    設定に環境が無いときに推測する。カレントディレクトリから lakefile を持つ親
    ディレクトリを探し、その lean_lib を import する。Lake プロジェクトの外なら
    Lean 本体だけで起動する。
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

    優先順は 引数 > 名前付き環境 > default > カレントディレクトリの Lake プロジェクト。
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


def read_text(path: str) -> str | None:
    """読めなければ None。ファイルが無い場合と権限が無い場合を区別しない箇所で使う。"""
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        return None


def read_toolchain(path: str | None) -> str | None:
    """そのディレクトリの lean-toolchain。無ければ None。"""
    text = read_text(f"{path}/lean-toolchain") if path else None
    return text.strip() or None if text else None
