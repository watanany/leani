---
name: leani-dev
description: leani に機能を足す、動作を変える、不具合を直すときの手順。ストーリー、テスト、実装、SPEC、文書、just check、merge の順に進める。leani のコード、テスト、文書を変える作業を始めるときに使う。
---

# leani の開発の手順

決まりの本文は `DEVELOPMENT.ja.md` にある。このスキルは、その決まりを作業の順に並べたものである。
引数なしで `just` を実行すると、同じ流れとコマンドの一覧が出る。

## 手順

担当が「スクリプト」の手順では、`just` のレシピを実行して結果を読む。AI は同じことを手で行わない。

| 手順                                                                                   | 担当            |
| -------------------------------------------------------------------------------------- | --------------- |
| 1. `just status` で、今の枝と、手順から外れていることの知らせを見る                    | スクリプト      |
| 2. `just start <枝の名前>` で、`main-rs` から枝を切る                                  | スクリプト      |
| 3. `stories.toml` にストーリーを足すか、既にあるストーリーを選ぶ                       | AI              |
| 4. テストを書き、`cargo nextest run <名前>` でテストが失敗することを確かめる           | AI              |
| 5. テストの文を、スキル `leani-test-text` で見直す                                     | スクリプトと AI |
| 6. 実装を書き、`cargo nextest run <名前>` でテストが通ることを確かめる                 | AI              |
| 7. 文書を変えたら、スキル `leani-docs-sync` で、もう片方の言語の文書に同じ変更を入れる | スクリプトと AI |
| 8. `just fix` で整形し、`SPEC.ja.md` と `SPEC.en.md` を生成し直す                      | スクリプト      |
| 9. `just check` でテストと lint をすべて実行する                                       | スクリプト      |
| 10. Conventional Commits の形で、日本語の commit メッセージを書いて commit する        | AI              |
| 11. `just finish` で、`main-rs` に merge して枝を消す                                  | スクリプト      |

動作を変えないリファクタリングでは、3 から 5 を飛ばしてよい。

## 実行するときの注意

- `just finish` は、`just check` の前と後に、枝の状態と commit の件名を確かめる。確かめに通らなければ、理由を出して merge せずに止まる。中で `just check` を実行するので、timeout を 600000 ms にする
- `just fix` はファイルを書き換え、`just finish` は git の枝を操作する。どちらも sandbox の外で実行するので、許可の一覧 (`permissions.allow`) に入れず、毎回ユーザーの許可を得る
- `just fix` は `.claude/` の下のファイルを整形しない。`.claude/` の下のファイルは Edit で直し、`just check` で整形を確かめる

## hook からの知らせ

`.claude/settings.json` の hook (`crates/claude-hooks`) が、手順から外れたことを知らせる。

- ファイルを変えたとき: そのファイルに関係する知らせが届く
- 終わろうとしたとき: 知らせがあれば、1 回だけ止められる

知らせは強制ではない。直すか、直さない理由をユーザーへの返事に書いて続ける。
同じ知らせは、1 つのセッションの中で繰り返さない。

hook は、ユーザーが `cargo install --path crates/claude-hooks` でインストールした版で動く。
`crates/claude-hooks`、`crates/spec`、`crates/docs-pair` を変えたら、インストールし直すようにユーザーに伝える。
`just finish` も、merge で変わったファイルを見て、インストールし直しを頼む文を出す。
AI が自分でインストールし直さない。
