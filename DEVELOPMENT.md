# 開発メモ

leani を手元で直すときに気をつけていることのメモ。

leani は Rust で書いてある (`crates/`)。Jupyter のカーネルだけは Python 版 (`src/leani/`)
で動いている。Python 版はカーネルのためだけに残してあり、カーネルの不具合だけを直す。

## インストール

clone したリポジトリからインストールする場合は、リポジトリの中で次のコマンドを実行する。
Rust の toolchain が必要。

```
cargo install --path crates/leani
```

コードを変えたら、このコマンドを再実行する。インストールせずに試すときは
`cargo run -p leani -- <引数>` を使う。

Jupyter のカーネルは、次のコマンドでインストールする。この方法でインストールすると、
Python のコードの変更は次の起動から反映される。

```
uv tool install --editable '.[jupyter]' --force
leani-kernel install
```

## テストと lint

`just check` で、次に挙げるテストと lint をすべて実行する。`just` は `brew install just` で
インストールする。

テストには、Rust の toolchain、cargo-nextest、elan、ビルド済みのエンジンが必要。
エンジンは `cargo run -p leani -- --setup` でビルドする。

```
cargo nextest run                          # すべてのテストを実行する
cargo nextest run -E 'binary(parsing)'     # 純粋関数と読み取りのテストだけを実行する
cargo nextest run 履歴                     # テストを名前で絞り込む
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

テストを動かす正式な方法は `cargo nextest run` である。nextest はテストごとに別の
プロセスで動かす。`cargo test` でも通るが、同じプロセスの中でテストが並列に動く。

Jupyter のカーネルのテストと lint は、次のコマンドで実行する。

```
uv sync
uv run pytest -n auto
uv run ruff format . && uv run ruff check .
uv run mypy
```

テストの名前やストーリーを変えたら、`cargo run -p spec` を実行して `SPEC.md` を生成し直す。
カーネルのテスト (`tests/test_kernel.py`) を変えたときも同じ。
`SPEC.md` の内容がテストと一致していないと、テストが失敗する。

## 作業の流れ

- 機能を追加するときは、ストーリー、テスト、実装の順に書く。このリポジトリでは、この 3 つのつながりがいちばん大事である。ストーリーは `stories.toml` に追加し、テストには `#[story("A1")]` でストーリーの ID を付ける (カーネルのテストでは `@story("A1")`)。1 つのストーリーに複数のテストを付けてよい
- テストは `crates/leani/tests/` に置く。`crates/spec` がこのディレクトリと `tests/test_*.py` だけを読んで `SPEC.md` を作るので、crate の中の単体テストは `SPEC.md` に載らない。テストの関数名は日本語の文で書き、`mod` を describe にする
- コードを変えたら、変えた関数の doc comment、呼び出し元と呼び出し先のコメント、その動作を説明しているドキュメントを読み直す。テストと `SPEC.md` のずれは `crates/spec` が検出するが、コメントとドキュメントのずれを検出するものは無い。動作の説明のうち、入力と出力で確かめられるものは、コメントではなくテストの名前に書く
- 作業はブランチで行い、終わったら main に merge してブランチを消す。PR は作らない
- commit メッセージは Conventional Commits の形で、日本語で書く

## コードの書き方

crate は副作用の有無で分けてある。

| crate        | 中身                                                     |
|--------------|----------------------------------------------------------|
| `leani-core` | 純粋な関数と定数。入力だけで出力が決まる                 |
| `leani-read` | 読み取り。環境変数とファイルを読むが、何も書き換えない   |
| `leani`      | 副作用。プロセス、端末、ファイル、ネットワークを操作する |
| `story`      | テストに付ける `#[story]`。何もせずに関数を返す          |
| `spec`       | `SPEC.md` を生成する                                     |

各 crate のモジュールの一覧は、その crate の `lib.rs` の先頭に書いてある。
コードを追加するときは、次の決まりに従う。

- 原則として、純粋関数型の考え方でコードを書く。基本的な書き方は Haskell を意識する。値を書き換えるより新しい値を返し、小さな関数を組み合わせる
- 分岐は、書ける箇所では `match` で書く。不自然になるなら `if` を式として書く
- 純粋関数と読み取りの関数では、`if` の `else` を省略しない。どの分岐でも値を返すことが、コードの形でわかるようにする
- 副作用のある関数では、`else` を省略してよい。Go のように、条件に合わなければ先に return する
- 判定と整形は `leani-core` に書く。`leani` には、プロセス、端末、ファイルの操作だけを書く。こうすると、判定と整形のテストを入力と出力だけで書ける
- 依存の向きは `leani-core` ← `leani-read` ← `leani` だけにする
- プロセス全体の状態 (環境変数、カレントディレクトリ、SIGINT のハンドラ) に触るのは `main.rs` だけにする。`main.rs` が起動時に 1 回だけ環境変数を読んで `Ctx` を作り、ほかの関数は `Ctx` を引数で受け取る。lib は `std::env::set_var` と `set_current_dir` を呼ばない。子プロセスには `Command::env` と `current_dir` で渡す
- セッションの状態 (エンジンのプロセス、宣言のログ、入力バッファなど) を持つのは `Engine` と `Repl` だけにする。`Engine` のフィールドは private にして、`Engine` のメソッドだけが書き換える
- 表示は `print!` で出さず、`Repl` に渡した `Output` (`self.out`) の `write` と `fail` で出す。テストでは、文字列にためるだけの型が表示を受け取る
- テストがエンジンとのやりとりを差し替えるためのメソッドは、`testing` feature のときだけ作る
- lint は `clippy` の一般的な設定に従う。ただし、このリポジトリの決まりと合わないときは、このリポジトリの決まりを優先する

Jupyter のカーネル (Python 版) のモジュールの決まりは、`src/leani/__init__.py` の docstring と
`pyproject.toml` の `ruff` の設定に書いてある。

## 文書の書き方

- 常体 (だ・である) で書く
- 英数字の前後には半角の空白を入れる。`` ` `` で囲んだコードの前後も同じにする
- 主語と目的語を省かない。「入る」のように、何がどうなるか分からない書き方をしない
- 比喩と珍しい語を使わない。指摘されたら、`rg` で同じ書き方を探してまとめて直す
- 必要なことだけを書き、細かいことを重要なことと同じ重さで並べない。README は入口なので、経緯と手元の環境のパスを書かない

## ドキュメント

| ファイル                                           | 内容                                                                   |
|----------------------------------------------------|------------------------------------------------------------------------|
| [README.md](README.md)                             | インストールと、はじめての起動                                         |
| [guide/proof.md](guide/proof.md)                   | 証明モード                                                             |
| [guide/config.md](guide/config.md)                 | 設定、環境変数、ファイルの場所                                         |
| [guide/jupyter.md](guide/jupyter.md)               | Jupyter のカーネルとして使う方法                                       |
| [SPEC.md](SPEC.md)                                 | leani ができることの一覧。`crates/spec` がテストから生成する           |
| [DECISIONS.md](DECISIONS.md)                       | 設計の判断。採用しなかった案と、その理由                               |
| `stories.toml`                                     | ユーザーストーリーの一覧                                               |
| `crates/`                                          | leani のコード。設計の理由は doc comment とコメントに書いてある        |
| `src/leani/`                                       | Jupyter のカーネル (Python 版)                                         |
