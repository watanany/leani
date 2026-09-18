"""起動の用意 (副作用)。

Lean のバージョンを決め、エンジン (leanprover-community/repl) を取ってきてビルドし、
lake の環境変数を用意する。"""

from __future__ import annotations

import contextlib
import hashlib
import os
import re
import shutil
import subprocess
from collections.abc import Sequence

from leani.config import EnvConfig, read_text, read_toolchain
from leani.places import ENGINE_REPO, NO_SETUP, SETUP_ENV, SETUP_TIMEOUT, STATE
from leani.pure import (
    dim,
    engine_dir,
    parse_env_lines,
    pick_tag,
    toolchain_version,
    version_key,
)
from leani.types import EngineError

LEAN_VERSION = re.compile(r"version (\d+\.\d+\.\d+(?:-rc\d+)?)")


def local_toolchain() -> str | None:
    """elan が今選んでいるバージョン。プロジェクトも指定も無いときの落とし所。"""
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
    使う Lean のバージョン。プロジェクト → 明示されたエンジン → elan の既定。

    2 番目は手動で指したエンジン用。leani はそれをビルドし直さないので、
    合わせるべきバージョンは「そのエンジンをビルドしたバージョン」しかない。leani が
    用意したエンジンなら engine が None なので、ここは飛ばして既定バージョンに落ちる。
    """
    return (
        read_toolchain(cfg.project) or read_toolchain(cfg.engine) or local_toolchain()
    )


def toolchain(cfg: EnvConfig) -> str:
    """
    使うバージョンを決める。分からなければ起動を中止する。

    lean は cwd の lean-toolchain を見てバージョンを決めるので、プロジェクトの外から
    呼ぶと既定のバージョンが選ばれて olean が読めなくなる。ここで決めたバージョンを
    elan run で固定し、エンジンもそのバージョンでビルドする。
    """
    tc = guess_toolchain(cfg)
    if tc is None:
        raise EngineError("Lean のバージョンが分からない (lean --version が答えない)")

    return tc


def manual_setup(path: str, tc: str, tag: str | None) -> str:
    """
    自動で用意できなかったときに出す手順。

    タグを省くと HEAD が来てバージョンが合わないので、必ず指す。使うバージョンで
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
        # 同じプロセスグループなので Ctrl-C はこちらにも来る。起動の失敗として扱う。
        raise EngineError(f"^C 中断した: {' '.join(cmd)}") from e

    if r.returncode != 0:
        raise EngineError(f"失敗した (exit {r.returncode}): {' '.join(cmd)}")


def build_engine(path: str, tc: str, tag: str) -> None:
    """
    タグのソースを使うバージョンでビルドして置く。

    olean を読めるかはビルドに使った Lean のバージョンで決まるので、clone した
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
    使うエンジンのディレクトリ。leani が持つ分は無ければ用意する。

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
            # nightly や stable。バージョンとして読めないので比べようがない。repl は
            # master が最新の Lean に追いているので、そこで試す。
            tag = "master"
            print(dim(f"{version} に対応するタグは無い。master で試す"), flush=True)
        elif tag is None:
            raise EngineError(f"repl に {version} 用のタグが無い")

        build_engine(path, tc, tag)
    except EngineError as e:
        # 自動で用意できなくても手動なら通ることがある。使うタグまで出しておく。
        raise EngineError(f"{e}\n{manual_setup(path, tc, tag)}") from e

    return path


LAKE_ENV_KEYS = ("LEAN_PATH", "LEAN_SRC_PATH", "DYLD_LIBRARY_PATH", "LD_LIBRARY_PATH")


def lake_env(project: str | None, tc: str = "") -> dict[str, str]:
    """
    lake env が解決する環境変数。

    lake env は起動に 1 秒近くかかるので、新しいキャッシュがあれば使い回す。
    LEAN_PATH は core の olean を指すので、toolchain ごとに別の鍵で持つ。
    同じ鍵で持ち回していたころは、rc を差し替えると前のバージョンの olean を指した
    ままになり、repl は起動するのに import が丸ごと落ちていた。
    """
    if not project:
        return {}

    key = hashlib.sha1(f"{project}\n{tc}".encode()).hexdigest()[:12]
    cache = f"{STATE}/lake-env/{os.path.basename(project)}-{key}"
    if lake_env_stale(cache, project):
        write_lake_env(cache, project)

    return parse_env_lines(read_text(cache) or "")


# キャッシュより新しければ取り直す。manifest は依存のバージョン、lean-toolchain は
# core のバージョン。どちらが動いても LEAN_PATH は変わる。
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
        raise EngineError(f"lake env を保存できなかった ({cache}): {e}") from e
    finally:
        with contextlib.suppress(OSError):
            os.unlink(tmp)


def prepare(cfg: EnvConfig) -> None:
    """
    その環境で起動できるようにする。エンジンが無ければここで用意する。

    Engine を作る前に呼べる。:env の切り替えでは今のエンジンを終了させる前に
    通すので、ここでエラーにできた分は何も壊さずに済む。lake env も先に解決して
    キャッシュしておく (Engine の中で失敗させない)。
    """
    tc = toolchain(cfg)
    ensure_engine(cfg.engine, tc)
    lake_env(cfg.project, tc)
