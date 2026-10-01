# leani

Lean 4 の REPL。

![leani で式を評価して補完の候補を表示したところ](docs/screenshot.png)

## インストール

elan、git、uv が必要。

```sh
uv tool install git+https://github.com/watanany/leani
```

## 使い方

```sh
leani              # 起動する
leani -i Mathlib   # Mathlib を import して起動する
```

初回は leanprover-community/repl をビルドするので、時間がかかる。
Lake プロジェクトの中では、先に `lake build` (Mathlib なら `lake exe cache get`) を実行しておく。
REPL の中では `:help` でコマンドの一覧を表示する。

[証明モード](docs/proof.md) · [設定](docs/config.md) · [Jupyter](docs/jupyter.md) · [開発](CONTRIBUTING.md)
