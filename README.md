<div align="center">

# leani

**Lean 4 を、その場で試す REPL**

<img src="guide/screenshot.png" alt="leani で式を評価して補完の候補を表示したところ" width="720">

[証明モード](guide/proof.md) · [設定](guide/config.md) · [Jupyter](guide/jupyter.md) · [開発メモ](DEVELOPMENT.md)

</div>

## インストール

[elan](https://lean-lang.org/install)、git、[Rust の toolchain](https://rustup.rs/) が必要。

```sh
cargo install --git https://github.com/watanany/leani leani
```

`uv tool install` でインストールした Python 版の leani がある場合は、先に `uv tool uninstall leani` で消す。

## 使い方

```sh
leani              # 起動する
leani -i Mathlib   # Mathlib を import して起動する
```

REPL の中では `:help` でコマンドの一覧を表示する。

> [!NOTE]
> 初回は leanprover-community/repl をビルドするので、時間がかかる。
> Lake プロジェクトの中では、先に `lake build` (Mathlib なら `lake exe cache get`) を実行しておく。
> Lake プロジェクトの中で初めて起動したときは、`lake env` が依存パッケージを取得することがある。
