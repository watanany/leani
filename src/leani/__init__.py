"""
leani: Lean 4 の対話 REPL。

エンジンは leanprover-community/repl。repl は JSON を受け取って JSON を返す
プログラム向けのプロトコルなので、leani が人間向けの層を担当する。

設計上の要点:

* **入力の完結判定は Lean のパーサに任せる。** 「入力が終わったか」を正規表現で
  近似すると、想定外の構文があるたびに早く確定しすぎるか、次の行まで同じ入力と
  して扱ってしまう。`Parser.runParserCategory` を今の環境で実行し、入力を
  command / term / tacticSeq としてパースできるかを確かめる。入力そのものは実行しない。
  ユーザー定義の notation も認識される。
* **エンジンが異常終了してもセッションは続く。** leani は受理した宣言のログを持ち、
  エンジンが異常終了したら再起動して replay する。評価中の Ctrl-C も同じ方法で復帰する。
* **セッションはソースコードとして保存する。** repl には環境を pickle する機能が
  あるが、復元した環境で `#eval` すると Lean のコンパイラが PANIC する。`:save` は
  受理した宣言を `.lean` ファイルに書き出す。そのファイルは `:l` でも lean 本体でも
  読み込める。

読み方: 副作用の有無でモジュールを分けてある。ラベルはこの 4 つ。

    純粋      入力だけで出力が決まる。同じ入力なら常に同じ結果
    読み取り  ファイルや環境変数を見るが、何も書き換えない
    副作用    プロセス、端末、ファイルを操作する。上から下へ読む
    定数      エンジンに送るクエリの文字列。操作するものは無い

    types    純粋      型と例外
    places   読み取り  設定、履歴、キャッシュのパス
    queries  定数      エンジンに送るクエリ
    abbrev   純粋      略記表、展開、一覧の整形
    pure     純粋      判定と整形
    config   読み取り  設定
    boot     副作用    起動の用意
    engine   副作用    repl プロセス
    search   副作用    loogle に問い合わせる
    show     副作用    表示を出す
    repl     副作用    フロントエンド
    cli      副作用    エントリーポイント
    kernel   副作用    Jupyter のカーネル (leani[jupyter] のときだけ使える)

純粋、読み取り、定数のモジュールが副作用のモジュールを import しないことは、
`ruff` が検査している (pyproject.toml の flake8-tidy-imports)。ただし `import leani`
はこのモジュールを通して kernel 以外のすべてのモジュールを読み込むのに、`ruff`
では検出できない。純粋、読み取り、定数のモジュールには書かない。判定と整形はすべて
純粋なモジュールにあるので、テストは入力と出力だけで書ける。セッションの状態を
持つのは、repl プロセスを 1 つ持つ `Engine` と、入力バッファと証明モードを持つ
`Repl` の 2 つだけ。`Engine` の属性は `Engine` のメソッドだけが書き換える。

このモジュールは名前をまとめるだけで、実装は持たない。
"""

from __future__ import annotations

from leani import (
    abbrev,
    boot,
    cli,
    config,
    engine,
    places,
    pure,
    queries,
    repl,
    search,
    show,
    types,
)
from leani.cli import main

__all__ = [
    "abbrev",
    "boot",
    "cli",
    "config",
    "engine",
    "main",
    "places",
    "pure",
    "queries",
    "repl",
    "search",
    "show",
    "types",
]
