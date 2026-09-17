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

読み方: 副作用の有無でモジュールを分けてある。札はこの 4 つ。

    純粋      入力だけで出力が決まる。同じ入力なら常に同じ結果
    読み取り  ファイルや環境変数を見るが、何も書き換えない
    副作用    プロセス・端末・ファイルを触る。上から下へ流れとして読む
    定数      エンジンに投げるクエリの文字列。触るものは無い

    types    純粋      型と例外
    places   読み取り  設定・履歴・キャッシュの置き場所
    queries  定数      エンジンに投げるクエリ
    abbrev   純粋      略記表と展開
    pure     純粋      判定と整形
    config   読み取り  設定
    boot     副作用    起動の用意
    engine   副作用    repl プロセス
    show     副作用    表示を出す
    repl     副作用    フロント
    cli      副作用    入口

純粋・読み取り・定数から副作用を import しないことは `ruff` が見ている
(pyproject.toml の flake8-tidy-imports)。判定と整形はすべて純粋な側にあるので、
テストは入力と出力だけで書ける。状態を持つのは repl プロセス 1 個を抱える
`Engine` と、入力バッファと証明モードを持つ `Repl` の 2 つだけ。

ここは名前をまとめるだけで、中身は持たない。
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
    "show",
    "types",
]
