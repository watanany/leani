---
name: leani-docs-sync
description: leani の英語の文書 X.md と日本語の文書 X.ja.md の片方だけを変えたときに、もう片方にも同じ変更を入れる。どちらの言語から変えてもよい。文書を変えたあとと、hook が対の文書の片方だけを変えたと知らせたときに使う。
---

# 対の文書をそろえる

README、DEVELOPMENT、DECISIONS、guide の文書は、英語の `X.md` と日本語の `X.ja.md` の 2 つがあり、どちらも原本である。
集める段と確かめる段はスクリプトで行い、訳す段だけを AI が行う。

## 1. 集める (スクリプト)

```
just status
git diff main-rs -- <変えた文書>
```

`just status` が「対になる ... を変えていない」と出した文書について、`main-rs` からの差分を見る。

## 2. 訳す (AI)

差分と同じ変更を、もう片方の文書の対応する場所に入れる。

- `glossary.toml` の語の組を使う
- 変えた所だけを訳し、ほかの文は書き換えない
- 日本語の文書は `DEVELOPMENT.ja.md` の「文書の書き方」に従う。英語の文書は、今の文書の書き方に合わせる

## 3. 確かめる (スクリプト)

```
just status
```

対の知らせが消えたことを確かめる。両方の文書を同じ commit に入れてから、`cargo run -p docs-pair -- --check` を実行する。
片方だけを変える理由があるときは、commit メッセージに `Docs-Pair-Skip: 理由` の行を書く。
