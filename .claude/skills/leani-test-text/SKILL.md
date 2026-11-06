---
name: leani-test-text
description: leani のテストの文 (doc comment と docstring の ja: と en:) を、DEVELOPMENT.ja.md の「テストの文の書き方」に照らして見直す。テストを足したか、テストの文を変えたあとに使う。
---

# テストの文の見直し

集める段と確かめる段はスクリプトで行い、判断する段と直す段だけを AI が行う。

## 1. 集める (スクリプト)

```
just texts
just texts-lint
```

`just texts` は、`main-rs` から足した `ja:` と `en:` の行を `ファイル:行: 文` の形で出し、`target/claude-hooks/texts.txt` にも書く。
見直すのは、この出力に出た行だけにする。

`just texts-lint` は、足した `ja:` の文を yomiyasu の lint にかけ、指摘を元の場所 (`ファイル:行`) で出す。
`DEVELOPMENT.ja.md` の決まりと食い違う規則の指摘は、スクリプトが外す。

## 2. 判断する (AI)

`DEVELOPMENT.ja.md` の「テストの文の書き方」を読んでから、`just texts` の出力の行ごとに、テストの中身 (assert) と照らして判断する。
`just texts-lint` の指摘も判断に使う。ただし、`DEVELOPMENT.ja.md` の決まりと食い違う直しは使わず、半角の () は残す。

判断は `target/claude-hooks/texts.json` に、出力の行ごとに 1 つの要素を持つ配列で書く。

```json
[
  {
    "place": "crates/leani/tests/parsing.rs:10",
    "text": "ja: 文を返す",
    "verdict": "ok"
  },
  {
    "place": "crates/leani/tests/parsing.rs:11",
    "text": "en: Returns a sentence",
    "verdict": "ng",
    "reason": "破った決まり",
    "fixed": "en: 直した文"
  }
]
```

| キー      | 中身                                                                  |
| --------- | --------------------------------------------------------------------- |
| `place`   | 出力の行の `ファイル:行` を写す                                       |
| `text`    | 出力の行の、`ja:` か `en:` から行の終わりまでを写す                   |
| `verdict` | `ok` か `ng`                                                          |
| `reason`  | `ng` のときだけ書く。破った決まり                                     |
| `fixed`   | `ng` のときだけ書く。直した文を、元の文と同じ `ja:` か `en:` から書く |

見る点は次のとおり。

- ja: Given / When / Then の順に読めるか。テストが確かめていないことを書いていないか
- en: Then の動詞 (3 人称単数) から始まり、ja の訳になっているか。`glossary.toml` の語を使っているか

## 3. 確かめる (スクリプト)

```
just texts-verify
```

`texts.json` の `place` と `text` の組が、`just texts` の今の出力と 1 つずつ合っているか、要素の形が合っているかを確かめる。
`fixed` が元の文と同じ `ja:` か `en:` で始まり、元の文と違うことも確かめる。
合っていなければ、理由を出して 1 で終わる。そのときは `texts.json` を直す。
`ng` の文があれば、理由と直した文の一覧を出して 1 で終わる。すべて `ok` なら 0 で終わり、見直しは終わる。

## 4. 直す (AI)

`ng` の文を、直した文に Edit で書き換える。`en:` の文を直したときは、テストの関数名か `mod` の名前も、`en:` の文から作り直す。
直したら `just fix` で `SPEC.ja.md` と `SPEC.en.md` を生成し直し、1 からやり直す。
