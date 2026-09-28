# エラーが出たとき

見出しは leani が表示するエラーメッセージである。
`…` はメッセージの続きを省いたところを表す。

## `import が通らない: …`

leani は、import したモジュールを読み込めなかった。
モジュール名が正しいかを確認する。
モジュール名が正しい場合は、Lake プロジェクトで `lake build` を実行する。
leani はプロジェクトをビルドしないので、`.olean` ファイルが無いと import に失敗する。
Mathlib を使う場合は、`lake exe cache get` を実行する。

`:l` で読み込んだファイルの import を解決できなかった場合、leani は `import が解決できないのでヘッダが丸ごと捨てられた` と表示する。
対処は同じである。

## `エンジンのバージョンがプロジェクトと違う …`

`env.<名前>.engine`、設定の `engine`、`LEANI_ENGINE` のどれかで指定したエンジンの Lean のバージョンが、Lake プロジェクトの Lean のバージョンと違う。
エラーメッセージに表示された手順でエンジンをビルドし直すか、その指定を外して leani にエンジンを用意させる。

## `repl が未ビルド: …`

指定したエンジンの repl がビルドされていない。
エラーメッセージに表示された `lake build repl` を実行する。

## `elan が PATH に無い` / `lake が PATH に無い`

leani は `elan` か `lake` のコマンドを見つけられなかった。
elan をインストールして、`~/.elan/bin` を PATH に追加する。

## `git が PATH に無い。…`

leani はエンジンを取得するのに git を使う。
git をインストールする。

## `Lean のバージョンが分からない`

leani は、プロジェクトの `lean-toolchain` からも `lean --version` の出力からも、Lean のバージョンを読み取れなかった。
Lake プロジェクトの外で起動した場合は、`elan default stable` などで elan のデフォルトの toolchain を設定する。
Lake プロジェクトの中で起動した場合は、`lean-toolchain` があるかを確認する。

## `env.<名前>.imports は配列で書く …`

設定ファイルの `imports` に、配列ではなく文字列を書いている。
`imports = ["Mathlib"]` のように、配列で書く。
ほかのキーの型が違う場合も、leani はキーの場所を表示して起動を中止する。

## `すでにある: …`

`:save` に渡したファイルがすでにある。
ファイルを消すか、別のファイル名を渡す。
