# 開発

## インストール

clone したリポジトリからインストールする場合は、リポジトリの中で次のコマンドを実行する。

```
uv tool install --editable . --force
```

`--force` は、git からインストールした leani をこの方法で置き換えるために付ける。
この方法でインストールすると、コードの変更は次の起動から反映される。
`pyproject.toml` の依存やエントリポイントを変えたときだけ、このコマンドを再実行する。

## テストと lint

```
uv sync
uv run pytest                         # すべてのテストを実行する
uv run pytest -n auto                 # テストを並列で実行する
uv run pytest tests/test_parsing.py   # 純関数のテストだけを実行する
uv run pytest -k 履歴                 # テストを名前で絞り込む
uv run ruff format . && uv run ruff check .
uv run mypy
```

`-n auto` で実行すると、端末のテストで forkpty の `DeprecationWarning` が表示される。
この警告はテストの結果に影響しない。

テストの名前やストーリーを変えたら、`uv run python tools/spec.py` を実行して `SPEC.md` を生成し直す。
`SPEC.md` の内容がテストと一致していないと、テストが失敗する。

## ドキュメント

| ファイル                                           | 内容                                                           |
|----------------------------------------------------|----------------------------------------------------------------|
| [README.md](README.md)                             | インストールと、はじめての起動                                 |
| [docs/usage.md](docs/usage.md)                     | 機能、コマンド、起動オプション                                 |
| [docs/config.md](docs/config.md)                   | 設定、環境変数、ファイルの場所                                 |
| [docs/troubleshooting.md](docs/troubleshooting.md) | エラーが出たときの対処                                         |
| [SPEC.md](SPEC.md)                                 | leani ができることの一覧。`tools/spec.py` がテストから生成する |
| [REJECTED.md](REJECTED.md)                         | 採用しなかった案と、その理由                                   |
| `tests/stories.py`                                 | ユーザーストーリーの一覧                                       |
| `src/leani/`                                       | leani のコード。設計の理由は docstring とコメントに書いてある  |
