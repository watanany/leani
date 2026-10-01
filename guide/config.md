# 設定

設定ファイルは `~/.config/leani/config.toml` に置く。
無くても leani は動く。

## 環境

Lake プロジェクトと import の組み合わせに名前を付けて、**環境** として登録できる。

```toml
default = "main"

[env.main]
project = "~/src/my-project"
imports = ["MyProject"]

[env.math]
project = "~/src/my-project"
imports = ["Mathlib"]
prompt = "λ∀> "
```

| キー                 | 説明                                                                |
|----------------------|---------------------------------------------------------------------|
| `default`            | オプションを付けずに起動したときの環境の名前                        |
| `engine`             | 自分でビルドしたエンジン (leanprover-community/repl) のディレクトリ |
| `env.<名前>.project` | Lake プロジェクトのディレクトリ                                     |
| `env.<名前>.imports` | import するモジュールの配列                                         |
| `env.<名前>.prompt`  | プロンプトの文字列                                                  |
| `env.<名前>.engine`  | その環境だけで使うエンジンのディレクトリ                            |

エンジンを自分で指定すると、`env.<名前>.engine`、`engine`、環境変数 `LEANI_ENGINE` の順に優先される。
指定しなければ、leani が自動でビルドする。

## 起動する環境の決まり方

`leani -e math` のように名前を指定すると、その環境で起動する。
`-e` を付けずに `-p` や `-i` を付けると、設定の環境を使わずに、指定したプロジェクトと import で起動する。
何も付けなければ `default` の環境で起動し、`default` が無ければ、カレントディレクトリを含む Lake プロジェクトで起動する。
REPL の中では `:env <名前>` で環境を切り替えられる。

## init ファイル

`~/.config/leani/init.lean` に書いた宣言は、起動のたびに実行される。
`:reset` や `:l` のあとも残る。

## 環境変数

| 環境変数         | 説明                                                         |
|------------------|--------------------------------------------------------------|
| `LEANI_CONFIG`   | 設定ファイルのパス                                           |
| `LEANI_INIT`     | init ファイルのパス                                          |
| `LEANI_HISTORY`  | 履歴ファイルのパス                                           |
| `LEANI_ENGINE`   | 自分でビルドしたエンジンのディレクトリ                       |
| `LEANI_NO_SETUP` | 空でない値にすると、エンジンを自動でビルドしない             |
| `LEANI_LOOGLE`   | `:loogle` が問い合わせる URL                                 |

## ファイルの場所

| ファイル | 場所                                       |
|----------|--------------------------------------------|
| 設定     | `~/.config/leani/config.toml`              |
| init     | `~/.config/leani/init.lean`                |
| 履歴     | `~/.local/state/leani/history`             |
| エンジン | `~/.local/state/leani/engine/<バージョン>` |

`XDG_CONFIG_HOME` と `XDG_STATE_HOME` を設定していれば、そちらが使われる。
実際の場所は `leani -V` で確認できる。
