# 設定

## 環境

leani では、Lake プロジェクトと import の組み合わせを **環境** と呼ぶ。
環境は設定ファイル `~/.config/leani/config.toml` の `[env.<名前>]` に書く。
設定ファイルは無くてもよい。

```toml
default = "main"
engine = "~/src/lean-repl"   # 自分で用意したエンジンを使う場合

[env.main]
project = "~/src/my-project"
imports = ["MyProject"]

[env.math]
project = "~/src/my-project"
imports = ["MyProject.Analysis"]
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

## 起動する環境の決まり方

leani は、次の順に起動する環境を決める。

1. `-e <名前>` を付けた場合は、その名前の環境で起動する。`-p` や `-i` も付けた場合は、その値で環境の `project` や `imports` を置き換える
2. `-p` か `-i` を付けた場合は、設定の環境を使わず、指定した Lake プロジェクトと import で起動する。`default` は使わない。`-i` だけを付けた場合、leani は 4 と同じ方法でカレントディレクトリから Lake プロジェクトを探す。`-p` だけを付けた場合、leani はそのプロジェクトの `lean_lib` を import する
3. オプションを付けず、設定ファイルに `default` がある場合は、`default` の環境で起動する
4. どれにも当てはまらない場合、leani はカレントディレクトリから `lakefile.toml` か `lakefile.lean` を持つ一番近い親ディレクトリを探し、その Lake プロジェクトの `lean_lib` を import して起動する。Lake プロジェクトが見つからなければ、Lean 本体だけで起動する

## 自分で用意したエンジンを使う

leani は通常、エンジンを自動でビルドする。
自分でビルドしたエンジンを使う場合は、そのディレクトリを次のどれかで指定する。
上にあるものほど優先される。

1. `env.<名前>.engine`
2. 設定ファイルの `engine`
3. 環境変数 `LEANI_ENGINE`

`-p` や `-i` で起動した場合は、`env.<名前>.engine` は使われず、`engine` か `LEANI_ENGINE` が使われる。

指定したエンジンのディレクトリには、`lean-toolchain` と、ビルド済みの repl (`.lake/build/bin/repl`) が必要である。
repl は、エンジンのディレクトリで `lake build repl` を実行するとビルドできる。
leani は指定されたエンジンをビルドしない。
エンジンの Lean のバージョンがプロジェクトと違うと、leani はエンジンをビルドし直す手順を表示して起動を中止する。
Lake プロジェクトの外で起動した場合、leani はエンジンの `lean-toolchain` に書かれたバージョンの Lean を使う。

## init ファイル

leani は起動時に `~/.config/leani/init.lean` の宣言を実行する。
init ファイルの宣言は、`:reset` や `:l` のあとも残る。
init ファイルを書き換えた内容は、次の起動から反映される。
init ファイルの `import` の行は無視される。

## 環境変数

| 環境変数         | 説明                                                                                                                                       |
|------------------|--------------------------------------------------------------------------------------------------------------------------------------------|
| `LEANI_CONFIG`   | 設定ファイルのパス                                                                                                                         |
| `LEANI_INIT`     | init ファイルのパス                                                                                                                        |
| `LEANI_HISTORY`  | 履歴ファイルのパス                                                                                                                         |
| `LEANI_ENGINE`   | 自分でビルドしたエンジンのディレクトリ。設定ファイルの `engine` のほうが優先される                                                         |
| `LEANI_NO_SETUP` | 空でない値を設定すると、leani は起動時にエンジンを自動でビルドせず、手順を表示して終了する。`leani --setup` はこの設定に関係なくビルドする |
| `LEANI_LOOGLE`   | `:loogle` が問い合わせる URL。デフォルトは `https://loogle.lean-lang.org/json`                                                             |

## ファイルの場所

| ファイル                | 場所                                       |
|-------------------------|--------------------------------------------|
| 設定                    | `~/.config/leani/config.toml`              |
| init                    | `~/.config/leani/init.lean`                |
| 履歴                    | `~/.local/state/leani/history`             |
| `lake env` のキャッシュ | `~/.local/state/leani/lake-env/`           |
| エンジン                | `~/.local/state/leani/engine/<バージョン>` |

`XDG_CONFIG_HOME` や `XDG_STATE_HOME` を設定している場合、leani は `~/.config` と `~/.local/state` の代わりにそのディレクトリを使う。
`leani -V` を実行すると、実際に使っている場所を確認できる。
