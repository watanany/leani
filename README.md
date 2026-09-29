# leani

![leani で式を評価して補完の候補を表示したところ](docs/screenshot.png)

leani は Lean 4 の REPL である。
ファイルを作って `lake env lean` を実行しなくても、式の値や宣言の結果をその場で確かめられる。
エンジンには leanprover-community/repl を使う。

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

leani は macOS と Linux で使える。

## インストール

leani を使うには、次のソフトウェアが必要である。

- elan (Lean と `lake` を管理するツール)。インストール方法は https://lean-lang.org/install を参照
- git (leani がエンジンを取得するときに使う)
- uv。インストール方法は https://docs.astral.sh/uv/getting-started/installation/ を参照。Python 3.11 以上が無い場合は、uv が Python を用意する

次のコマンドで leani をインストールする。

```
uv tool install git+https://github.com/watanany/leani
```

uv は `leani` コマンドを `~/.local/bin` に置く。
`~/.local/bin` が PATH に無い場合は、`uv tool update-shell` を実行する。

## はじめての起動

Lake プロジェクトの外で `leani` を実行すると、leani は elan のデフォルトの Lean で起動する。
`1 + 1` を入力して `2` が表示されれば、準備は完了している。

leani は Lean のバージョンごとに、初回の起動時にエンジンを GitHub から clone してビルドする。
このため、初回の起動にはネットワークが必要で、時間もかかる。
先にビルドだけ済ませたい場合は `leani --setup` を実行する。

Lake プロジェクトの中で `leani` を実行すると、leani はそのプロジェクトの `lean_lib` を import して起動する。
leani はプロジェクトをビルドしないので、先にプロジェクトで `lake build` を実行しておく。

Mathlib を使う場合は、Mathlib を依存に持つ Lake プロジェクトを用意する (`lake new <名前> math` で作れる)。
そのプロジェクトで次のコマンドを実行する。

```
lake exe cache get
leani -i Mathlib
```

Mathlib を import すると、起動に 6〜11 秒かかる。
`lake exe cache get` か `lake build` を実行していないと、leani は `import に失敗した` と表示して起動を中止する。

## よく使うコマンド

REPL の中で `:help` を実行すると、すべてのコマンドを確認できる。

| コマンド           | 説明                                                      |
|--------------------|-----------------------------------------------------------|
| `:t <expr>`        | 式の型を表示する (`#check`)                               |
| `:l <file>` / `:r` | ファイルを読み込む / 読み込み直す                         |
| `:prove`           | `sorry` の証明モードを始める。タクティクを 1 つずつ試せる |
| `:save <file>`     | 実行した宣言を `.lean` ファイルに書き出す                 |
| `:q`               | leani を終了する (Ctrl-D でも終了する)                    |

## ドキュメント

- [使い方](docs/usage.md): 機能、すべてのコマンド、証明モード、起動オプション
- [設定](docs/config.md): 設定ファイル、init ファイル、環境変数、ファイルの場所
- [エラーが出たとき](docs/troubleshooting.md): エラーメッセージごとの対処
- [開発](CONTRIBUTING.md): テストと lint の実行方法、開発用のドキュメント
