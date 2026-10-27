"""テストの共通部分。

Python のテストは、Jupyter のカーネルのテスト (test_kernel.py) だけ。端末の REPL の
テストは Rust 版 (`crates/leani/tests/`) にある。leani のコードは src/leani/ にある。

テストは Lean 本体だけを import する環境で実行する。特定の Lake プロジェクトに依存
しないので、このリポジトリの外に持ち出してもそのまま実行できる。
"""

import os
import sys
import tempfile
import textwrap
import tomllib

import pytest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src")
sys.path.insert(0, SRC)

# 設定、履歴、init のパスは import 時に定数になるので、テストが leani を import する
# 前に決める。conftest.py はテストのモジュールより先に読み込まれる。
_STATE = tempfile.mkdtemp(prefix="leani-test-")
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

with open(os.path.join(ROOT, "stories.toml"), "rb") as f:
    STORIES = {s["id"] for s in tomllib.load(f)["story"]}


# ---------------------------------------------------------- ストーリーの指定


def story(*ids):
    """
    このテストが対象にするユーザーストーリー (stories.toml) を指定する。

    テストの中身にも分け方にも影響しない。ID が stories.toml にあるかは
    import するときに確認する。crates/spec は SPEC.md を生成するとき、この指定を
    行単位で読む。1 つのテストが複数のストーリーを対象にしてよく、
    1 つのストーリーを複数のテストが対象にしてよい。ストーリーはテストの目次ではなく、
    テストが足りているかを数える表なので、対応は N:M になる。
    """
    unknown = [i for i in ids if i not in STORIES]
    if unknown:
        raise ValueError(f"stories.toml に無いストーリー: {unknown}")

    def mark(fn):
        return fn

    return mark


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
