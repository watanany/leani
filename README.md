<div align="center">

# leani

**Lean 4 を、その場で試す REPL**

<img src="docs/screenshot.png" alt="leani で式を評価して補完の候補を表示したところ" width="720">

[証明モード](docs/proof.md) · [設定](docs/config.md) · [Jupyter](docs/jupyter.md) · [開発メモ](DEVELOPMENT.md)

</div>

## インストール

[elan](https://lean-lang.org/install)、git、[uv](https://docs.astral.sh/uv/) が必要。

```sh
uv tool install git+https://github.com/watanany/leani
```

## 使い方

```sh
leani              # 起動する
leani -i Mathlib   # Mathlib を import して起動する
```

REPL の中では `:help` でコマンドの一覧を表示する。

> [!NOTE]
> 初回は leanprover-community/repl をビルドするので、時間がかかる。
> Lake プロジェクトの中では、先に `lake build` (Mathlib なら `lake exe cache get`) を実行しておく。
