<div align="center">

# leani

**Lean 4 のための、気持ちよく書ける REPL**

![Lean 4](https://img.shields.io/badge/Lean-4-0b2e4e)
![Python](https://img.shields.io/badge/Python-3.11%2B-3776ab?logo=python&logoColor=white)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-555)

[インストール](#インストール) · [証明モード](docs/proof.md) · [設定](docs/config.md) · [Jupyter](docs/jupyter.md) · [開発](CONTRIBUTING.md)

<img src="docs/screenshot.png" alt="leani で式を評価して補完の候補を表示したところ" width="720">

</div>

ファイルを作って `lake env lean` を実行しなくても、式の値や宣言の結果をその場で確かめられる。
エンジンには [leanprover-community/repl](https://github.com/leanprover-community/repl) を使う。

```
λ> 1 + 1
2
λ> def fib : Nat → Nat
 |   | 0 => 0
 |   | 1 => 1
 |   | n+2 => fib n + fib (n+1)
 |
λ> List.range 12 |>.map fib
[0, 1, 1, 2, 3, 5, 8, 13, 21, 34, 55, 89]
λ> Std.Time.PlainDateT<Tab>    → Std.Time.PlainDateTime に補完される
```

## 特徴

- **書いたらすぐ評価**。式の値がその場で出る。複数行の `def` もそのまま書ける
- **補完と略記**。Tab で名前を補完し、`\to` と打てば `→` になる
- **証明モード**。タクティクを 1 つずつ試せる
- **止めても消えない**。Ctrl-C で評価を止めても、それまでの宣言はそのまま使える
- **Lake プロジェクトと Mathlib**。プロジェクトの中で起動すれば、そのライブラリを使える
- **Jupyter**。カーネルとしても使える

## インストール

[elan](https://lean-lang.org/install)、git、[uv](https://docs.astral.sh/uv/getting-started/installation/) を入れてから、次を実行する。

```sh
uv tool install git+https://github.com/watanany/leani
```

`leani` コマンドが見つからなければ、`uv tool update-shell` を実行する。

## はじめる

```sh
leani                  # elan のデフォルトの Lean で起動する
leani --setup          # エンジンのビルドだけを先に済ませる
```

初回はエンジンをビルドするので、ネットワークが必要で、時間もかかる。
REPL の中では `:help` でコマンドの一覧を表示し、`:q` か Ctrl-D で終了する。
起動オプションは `leani -h` で確認できる。

Lake プロジェクトの中で使うときは、先に `lake build` しておく。
していないと、import に失敗して起動しない。
Mathlib を使うには、Mathlib を依存に持つ Lake プロジェクト (`lake new <名前> math`) で次を実行する。

```sh
lake exe cache get
leani -i Mathlib
```
