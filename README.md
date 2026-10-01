<div align="center">

# leani

**Lean 4 を 1 行ずつ試せる REPL**

![Lean 4](https://img.shields.io/badge/Lean-4-0b2e4e)
![Python](https://img.shields.io/badge/Python-3.11%2B-3776ab?logo=python&logoColor=white)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-555)

[インストール](#インストール) · [証明モード](docs/proof.md) · [設定](docs/config.md) · [Jupyter](docs/jupyter.md) · [開発](CONTRIBUTING.md)

<img src="docs/screenshot.png" alt="leani で式を評価して補完の候補を表示したところ" width="720">

</div>

leani は、Lean 4 の式や宣言を打ち込むと、その場で結果を返す REPL である。
試すたびにファイルを作って `lake env lean` を実行する必要はない。

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
```

## 特徴

- **補完と略記**。Tab で名前を補完し、`\to` と打てば `→` になる
- **[証明モード](docs/proof.md)**。`sorry` の箇所で、タクティクを 1 つずつ試せる
- **止めても消えない**。Ctrl-C で評価を止めても、それまでの宣言はそのまま使える
- **Lake プロジェクトと Mathlib**。プロジェクトの中で起動すると、そのライブラリを使える
- **[Jupyter](docs/jupyter.md)**。ノートブックのカーネルとしても使える

## インストール

先に [elan](https://lean-lang.org/install)、git、[uv](https://docs.astral.sh/uv/getting-started/installation/) を入れておく。

```sh
uv tool install git+https://github.com/watanany/leani
```

`leani` が見つからないと言われたら、`uv tool update-shell` を実行してから、シェルを開き直す。

## 使ってみる

```sh
leani
```

最初の起動では、leani が裏で使う [leanprover-community/repl](https://github.com/leanprover-community/repl) をダウンロードしてビルドする。
ネットワークが必要で、少し時間がかかる。
ビルドは Lean のバージョンごとに 1 回だけで、次からは省かれる。

REPL の中では、`:help` でコマンドの一覧を表示し、`:q` か Ctrl-D で終了する。

### Lake プロジェクトで使う

```sh
cd my-project
lake build    # leani はプロジェクトをビルドしないので、先に済ませておく
leani         # プロジェクトのライブラリを import して起動する
```

### Mathlib を使う

```sh
lake new mymath math    # Mathlib を使うプロジェクトを作る
cd mymath
lake exe cache get      # ビルド済みの Mathlib をダウンロードする
leani -i Mathlib
```
