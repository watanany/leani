"""テストの共通部分。

Python のテストは、Jupyter のカーネルのテスト (test_kernel.py) だけ。端末の REPL の
テストは Rust 版 (`crates/leani/tests/`) にある。leani のコードは src/leani/ にある。

テストは Lean 本体だけを import する環境で実行する。特定の Lake プロジェクトに依存
しないので、このリポジトリの外に持ち出してもそのまま実行できる。
"""

import atexit
import os
import shutil
import sys
import tempfile
import textwrap

import pytest

# stories は @story と、SPEC.md に載らないテストを見つける hook を持つ。
pytest_plugins = ["pytester", "stories"]

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src")
sys.path.insert(0, SRC)

# 設定、履歴、init のパスは import 時に定数になるので、テストが leani を import する
# 前に決める。conftest.py はテストのモジュールより先に読み込まれる。
# pytest-xdist では worker ごとに作るので、プロセスの終わりに消す。
_STATE = tempfile.mkdtemp(prefix="leani-test-")
atexit.register(shutil.rmtree, _STATE, ignore_errors=True)
os.environ["LEANI_INIT"] = os.path.join(_STATE, "no-such-init.lean")
os.environ["LEANI_HISTORY"] = os.path.join(_STATE, "history")
os.environ["LEANI_CONFIG"] = os.path.join(_STATE, "config.toml")

# 環境を 2 つ用意する。core がデフォルトで、wide は :env の切り替え先。Lake を経由
# しないので起動は 1 秒もかからない。
with open(os.environ["LEANI_CONFIG"], "w") as _f:
    _f.write(
        textwrap.dedent("""\
        default = "core"

        [env.core]
        imports = []

        [env.wide]
        imports = ["Lean.Elab.Frontend"]
        prompt = "λ+> "
        """)
    )

# ------------------------------------------------------------------ fixture


@pytest.fixture
def place(tmp_path, monkeypatch):
    """PATH とカレントディレクトリを、決まったファイルだけがある場所にする。"""
    bin_ = tmp_path / "bin"
    bin_.mkdir()
    for name in ("leanfoo", "leanbar"):
        (bin_ / name).write_text("")
        (bin_ / name).chmod(0o755)
    (bin_ / "leannoexec").write_text("")
    (tmp_path / "notes.lean").write_text("")
    (tmp_path / "notes.md").write_text("")
    monkeypatch.setenv("PATH", str(bin_))
    monkeypatch.chdir(tmp_path)
