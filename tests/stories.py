"""ストーリーの指定と、テストが SPEC に載る形かの確認。

crates/spec は tests/test_*.py を行単位で読み、describe_ の中で @story を付けた
関数だけを SPEC (SPEC.ja.md と SPEC.en.md) に載せる。conftest.py はこのモジュールの hook を使う。
"""

import os
import tomllib

import pytest
from pytest_describe.plugin import DescribeBlock

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

with open(os.path.join(ROOT, "stories.toml"), "rb") as f:
    STORIES = {s["id"] for s in tomllib.load(f)["story"]}


def story(*ids):
    """
    このテストが対象にするユーザーストーリー (stories.toml) を指定する。

    テストの中身にも分け方にも影響しない。ID が stories.toml にあるかは
    import するときに確認する。crates/spec は SPEC を生成するとき、この指定を
    行単位で読む。1 つのテストが複数のストーリーを対象にしてよく、
    1 つのストーリーを複数のテストが対象にしてよい。ストーリーはテストの目次ではなく、
    テストが足りているかを数える表なので、対応は N:M になる。
    """
    unknown = [i for i in ids if i not in STORIES]
    if unknown:
        raise ValueError(f"stories.toml に無いストーリー: {unknown}")

    def mark(fn):
        fn.stories = ids
        return fn

    return mark


def listed(item):
    """crates/spec が SPEC に載せる形のテストか。

    describe_ の関数の中 (入れ子でもよい) の関数で、@story が付いているもの。
    pytest-describe は describe の中の _ で始まらない関数をすべてテストとして集め、
    pytest は test で始まる関数と Test で始まる class を集める。spec は、このうち
    describe の外のものと @story の無いものを読まない。
    """
    chain = item.listchain()
    blocks = [n for n in chain if isinstance(n, DescribeBlock)]
    return (
        isinstance(item, pytest.Function)
        and bool(blocks)
        and item.parent is blocks[-1]
        and all(b.name.startswith("describe_") for b in blocks)
        and bool(getattr(item.function, "stories", ()))
    )


def pytest_runtest_setup(item):
    """SPEC に載らないテストを、実行する前に失敗させる。

    集めたときにエラーにすると、pytest-xdist の worker が落ちて理由が表示されない。
    """
    if not listed(item):
        pytest.fail(
            "SPEC に載らない。describe_ の中に書いて、@story を付ける",
            pytrace=False,
        )


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_makereport(item):
    """skip したテストと、xfail のテストを失敗にする。

    どちらも、確かめていないのに SPEC に載る。crates/spec は skip と xfail のマーカーを
    エラーにするが、テストの中で pytest.skip() を呼んだときは、結果を見ないと分からない。
    """
    report = (yield).get_result()
    if report.skipped or hasattr(report, "wasxfail"):
        report.outcome = "failed"
        report.longrepr = f"skip か xfail のテストは SPEC に載せられない: {item.nodeid}"
        if hasattr(report, "wasxfail"):
            del report.wasxfail
