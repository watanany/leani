"""起動の用意 (副作用)。

Lean のバージョンを決め、エンジン (leanprover-community/repl) を clone してビルドし、
lake の環境変数を用意する。"""

from __future__ import annotations

import contextlib
import hashlib
import os
import shutil
import subprocess
from collections.abc import Sequence

from leani.config import EnvConfig, read_text, read_toolchain
from leani.places import ENGINE_REPO, NO_SETUP, SETUP_ENV, SETUP_TIMEOUT, STATE
from leani.pure import (
    dim,
    engine_dir,
    is_channel,
    parse_env_lines,
    pick_tag,
    toolchain_of_version,
    toolchain_version,
    version_key,
)
from leani.types import EngineError


def lean_toolchain(args: Sequence[str]) -> str | None:
    """
    `lean <args>` が出すバージョンの toolchain。分からなければ None。

    elan は lean --version の中で toolchain を取得することがあるので、時間がかかる
    ことがある。時間を区切ると、取得が終わる前に「分からない」と表示することになる
    ので、区切らない。Ctrl-C で止める。
    """
    try:
        r = subprocess.run(
            ["lean", *args],
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError:
        return None

    return toolchain_of_version(r.stdout)


def local_toolchain() -> str | None:
    """elan が今選択しているバージョン。プロジェクトも指定も無いときに使う。"""
    return lean_toolchain(["--version"])


def guess_toolchain(cfg: EnvConfig) -> str | None:
    """
    使う Lean のバージョン。プロジェクト、ユーザーが指定したエンジン、elan の
    デフォルトの順に探す。
    """
    tc = read_toolchain(cfg.project) or read_toolchain(cfg.engine)
    if tc is None:
        return local_toolchain()
    if is_channel(tc) and cfg.engine is None:
        return lean_toolchain([f"+{tc}", "--version"]) or tc

    return tc


def toolchain(cfg: EnvConfig) -> str:
    """
    使うバージョンを決める。分からなければ leani は起動を中止する。
    """
    tc = guess_toolchain(cfg)
    if tc is None:
        raise EngineError(
            "Lean のバージョンが分からない "
            "(lean-toolchain が無く、lean --version からも取得できない)"
        )

    return tc


def manual_setup(path: str, tc: str, tag: str | None) -> str:
    """
    自動で用意できなかったときに表示する手順。
    """
    branch = tag or f"<{toolchain_version(tc)} 以下で一番新しいタグ>"
    return (
        f"手動で用意する場合:\n"
        f"  git clone --branch {branch} {ENGINE_REPO} {path}\n"
        f"  echo {tc} > {path}/lean-toolchain\n"
        f"  cd {path} && lake build repl"
    )


def fetch_tags() -> list[str]:
    """repl のタグ一覧。clone せずにリモートに問い合わせる。"""
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
        raise EngineError(
            f"repl のタグの取得が {SETUP_TIMEOUT} 秒で終わらなかった"
        ) from e
    except OSError as e:
        raise EngineError(f"git を実行できなかった: {e}") from e
    except KeyboardInterrupt as e:
        raise EngineError("^C 中断した") from e

    if r.returncode != 0:
        raise EngineError(f"repl のタグを取得できなかった:\n{r.stderr.strip()}")

    return [
        ref.rpartition("/")[2]
        for ref in r.stdout.split()
        if ref.startswith("refs/tags/")
    ]


def run_setup(cmd: Sequence[str], cwd: str | None = None) -> None:
    """エンジンの用意に使う外部コマンドを実行する。出力はそのまま表示する。"""
    print(dim(f"  {' '.join(cmd)}"), flush=True)
    try:
        r = subprocess.run(list(cmd), cwd=cwd, check=False, env=SETUP_ENV)
    except OSError as e:
        raise EngineError(f"{cmd[0]} を実行できなかった: {e}") from e
    except KeyboardInterrupt as e:
        # 同じプロセスグループなので、Ctrl-C は leani にも届く。起動の失敗として扱う。
        raise EngineError(f"^C 中断した: {' '.join(cmd)}") from e

    if r.returncode != 0:
        raise EngineError(f"失敗した (exit {r.returncode}): {' '.join(cmd)}")


def build_engine(path: str, tc: str, tag: str) -> None:
    """
    タグのソースを、使うバージョンでビルドして配置する。
    """
    version = toolchain_version(tc)
    note = "" if tag == version else f" (タグ {tag} を {version} でビルドする)"
    print(dim(f"エンジンを用意する (初回のみ): {version}{note}"), flush=True)

    # leani を 2 つ同時に起動しても衝突しないよう、作業用のディレクトリは
    # プロセスごとに分ける。
    tmp = f"{path}.{os.getpid()}.tmp"
    os.makedirs(os.path.dirname(path), exist_ok=True)
    try:
        run_setup(["git", "clone", "--depth", "1", "--branch", tag, ENGINE_REPO, tmp])
        with open(f"{tmp}/lean-toolchain", "w") as f:
            f.write(tc + "\n")

        run_setup(["lake", "build", "repl"], cwd=tmp)
        if not os.path.isfile(f"{tmp}/.lake/build/bin/repl"):
            raise EngineError(f"ビルドしたのに repl が無い: {tmp}")

        if os.path.isfile(f"{path}/.lake/build/bin/repl"):
            print(dim(f"別の leani がエンジンを用意していた: {path}"), flush=True)
            return

        shutil.rmtree(path, ignore_errors=True)
        os.replace(tmp, path)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    print(dim(f"用意した: {path}"), flush=True)


def ensure_engine(engine: str | None, tc: str, asked: bool = False) -> str:
    """
    使うエンジンのディレクトリを返す。leani が管理するエンジンは、無ければ用意する。

    ユーザーが engine を指定したときは leani の管理外なので、ビルド済みかを確認する
    だけで、何も作らないし消さない。build_engine は配置先のディレクトリを作り直すので、
    ユーザーが指定したディレクトリに対して呼んではいけない。
    """
    path = engine_dir(engine, tc)
    if os.path.isfile(f"{path}/.lake/build/bin/repl"):
        return path
    elif engine is not None:
        raise EngineError(f"repl が未ビルド: cd {path} && lake build repl")
    elif NO_SETUP and not asked:
        raise EngineError(
            f"エンジンが無い: {path}\n"
            f"  LEANI_NO_SETUP が設定されているので自動で用意しない "
            f"(leani --setup で用意する)\n{manual_setup(path, tc, None)}"
        )

    tag = None
    try:
        version = toolchain_version(tc)
        tag = pick_tag(version, fetch_tags())
        if tag is None and version_key(version) is None:
            tag = "master"
            print(dim(f"{version} に対応するタグは無い。master で試す"), flush=True)
        elif tag is None:
            raise EngineError(f"repl に {version} 用のタグが無い")

        build_engine(path, tc, tag)
    except EngineError as e:
        # 自動で用意できなくても、手動なら成功することがある。使うタグも含めて
        # 手順を表示する。
        raise EngineError(f"{e}\n{manual_setup(path, tc, tag)}") from e

    return path


LAKE_ENV_KEYS = ("LEAN_PATH", "LEAN_SRC_PATH", "DYLD_LIBRARY_PATH", "LD_LIBRARY_PATH")


def lake_env(project: str | None, tc: str = "") -> dict[str, str]:
    """
    lake env が設定する環境変数。

    lake env は起動に 1 秒近くかかるので、新しいキャッシュがあれば再利用する。
    LEAN_PATH は core の .olean を指すので、toolchain ごとに別のキーでキャッシュする。
    同じキーを使うと、rc のバージョンを変更したときに前のバージョンの .olean を
    指したままになり、repl は起動するのに import がすべて失敗する。
    """
    if not project:
        return {}

    key = hashlib.sha1(f"{project}\n{tc}".encode()).hexdigest()[:12]
    cache = f"{STATE}/lake-env/{os.path.basename(project)}-{key}"
    if lake_env_stale(cache, project):
        write_lake_env(cache, project)

    return parse_env_lines(read_text(cache) or "")


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
        # 確認しているあいだにファイルが削除されることがある。判断できなければ
        # 取得し直す。
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
            f"lake env が {SETUP_TIMEOUT} 秒で終わらなかった ({project})"
        ) from e
    except OSError as e:
        raise EngineError(f"lake env を実行できなかった ({project}): {e}") from e

    if r.returncode != 0:
        raise EngineError(f"lake env が失敗した ({project}):\n{r.stderr.strip()}")

    # 一時ファイルもプロセスごとに分ける。同時に起動した別の leani と同じ
    # ファイルに書き込まないため。
    tmp = f"{cache}.{os.getpid()}.tmp"
    try:
        os.makedirs(os.path.dirname(cache), exist_ok=True)
        with open(tmp, "w") as f:
            f.write(r.stdout)

        os.replace(tmp, cache)
    except OSError as e:
        raise EngineError(f"lake env を保存できなかった ({cache}): {e}") from e
    finally:
        with contextlib.suppress(OSError):
            os.unlink(tmp)


def prepare(cfg: EnvConfig) -> None:
    """
    その環境で leani を起動できるようにする。エンジンが無ければここで用意する。
    """
    tc = toolchain(cfg)
    ensure_engine(cfg.engine, tc)
    lake_env(cfg.project, tc)
