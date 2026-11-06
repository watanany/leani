---
name: leani-dev
description: leani に機能を足す、動作を変える、不具合を直すときの手順。ストーリー、テスト、実装、SPEC、文書、just check、merge の順に進める。leani のコード、テスト、文書を変える作業を始めるときに使う。
---

# leani の開発の手順

決まりの本文は `DEVELOPMENT.ja.md` にある。このスキルは、その決まりを作業の順に並べたものである。

## 手順

1. `just status` を実行して、今の枝と、手順から外れていることの知らせを見る
2. `main-rs` の上にいるなら、`git switch -c <枝の名前>` で枝を切る
3. 機能を足すときと動作を変えるときは、`stories.toml` にストーリーを足すか、既にあるストーリーを選ぶ
4. テストを書き、失敗することを確かめる。テストの文 (`ja:` と `en:`) を書いたら、スキル `leani-test-text` で見直す
5. 実装を書き、テストが通ることを確かめる
6. `cargo run -p spec` で `SPEC.ja.md` と `SPEC.en.md` を生成し直す
7. 文書を変えたら、スキル `leani-docs-sync` で、もう片方の言語の文書に同じ変更を入れる
8. `cargo fmt` と `uv run ruff format .` を実行してから、`just check` を実行する
9. Conventional Commits の形で、日本語で commit する
10. `just check` をもう一度実行する (`docs-pair` は commit だけを確かめる)
11. `git switch main-rs && git merge --ff-only <枝の名前> && git branch -d <枝の名前>`

動作を変えないリファクタリングでは、3 と 4 を飛ばしてよい。

## hook からの知らせ

`.claude/settings.json` の hook (`crates/claude-hooks`) が、手順から外れたことを知らせる。

- ファイルを変えたとき: そのファイルに関係する知らせが届く
- 終わろうとしたとき: 知らせがあれば、1 回だけ止められる

知らせは強制ではない。直すか、直さない理由をユーザーへの返事に書いて続ける。
同じ知らせは、1 つのセッションの中で繰り返さない。
