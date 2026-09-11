# leani

Lean 4 の対話 REPL。GHCi や IPython と同じように使える。

エンジンは leanprover-community/repl。repl は JSON in / JSON out の機械向け
プロトコルしか持たないので、leani がその上に人間向けの層を足す。

```
λ> 1 + 1
2
λ> def fib : Nat → Nat
  | | 0 => 0
  | | 1 => 1
  | | n+2 => fib n + fib (n+1)
  |
λ> List.range 12 |>.map fib
[0, 1, 1, 2, 3, 5, 8, 13, 21, 34, 55, 89]
λ> Std.Time.PlainDateT⇥        → Std.Time.PlainDateTime に補完される
```

## インストール

```
uv tool install git+https://github.com/watanany/leani
```

pipx でもいい。

```
pipx install git+https://github.com/watanany/leani
```

どちらも `leani` コマンドを `~/.local/bin` に置く。

必要なものは `elan` (Lean 本体と `lake`) と `git` の 2 つ。どちらかが PATH に
無ければ起動時に言う。Python は 3.11 以上が要る。

エンジンは leani が用意する。使う Lean の版ごとに
`~/.local/state/leani/engine/<版>` へ clone してビルドし、初回の起動だけ
10 秒ほど余分にかかる。先に済ませておくなら `leani --setup`、自動で用意させたく
なければ `LEANI_NO_SETUP=1` (手順を出して終わる)。

自分で clone したものを使うなら設定の `engine` か `LEANI_ENGINE` で指す。この
ときはビルドも版の管理も leani はしない。Lake プロジェクトの外なら、そのエンジンを
ビルドした版 (`lean-toolchain`) に合わせて起動する。プロジェクトと版が食い違う
ときは olean が読めず起動直後に落ちるだけなので、建て直す手順を出して断る。

## 環境の設定

「どの Lake プロジェクトの上で何を import して起動するか」を
`~/.config/leani/config.toml` に置く。GHCi の `~/.ghci`、IPython の profile と
同じ位置づけで、プロジェクトの名前はコードではなくここにだけ書く。

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

```
leani                  # default の環境
leani -e math          # 名前で選ぶ
leani -p . -i MyLib    # 設定を使わずその場で指定
leani foo.lean         # 読み込んで起動
leani --setup          # エンジンだけ用意して終わる
```

設定が無くても動く。cwd から `lakefile.toml` / `lakefile.lean` を持つ一番近い
親ディレクトリを探し、そこの `lean_lib` を import する。Lake プロジェクトの外なら
Lean 本体だけで起動する。`LEAN_PATH` は `lake env` を呼んで解決し、
`lake-manifest.json` と `lean-toolchain` のどちらより新しいキャッシュがあれば
使い回す (`lake env` は 1 秒近くかかる)。`LEAN_PATH` は core の olean も指すので、
キャッシュは toolchain ごとに分けて持つ。

`import Lean` は設定に関わらず必ず入る。完結判定と補完のクエリが
`Lean.Parser` と `CoreM` を使うため。

import が 1 つでも解決できないと、エンジンはヘッダを丸ごと捨てて (エラーも
出さずに) 起動してしまう。`import Lean` ごと落ちて何も通らない環境になるので、
起動したかを毎回確かめて、駄目なら断る。`:l` で読んだファイルの先頭の import も
同じように確かめる。設定の値の型が違うときも同じで、場所を言って断る
(`env.math.imports は配列で書く: ["Mathlib"]`)。

## できること

| 機能          | 中身                                                                                                                                                                                                          |
|---------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| 複数行入力    | 入力が終わったかを Lean のパーサに聞く (正規表現で近似しない)。インデントが続く限り読み、空行で確定。確定した直後にインデント行を書けば、直前の入力に遡って続きとして読み直す                                |
| 行編集・履歴  | readline (macOS は libedit)。履歴は `~/.local/state/leani/history` に毎行書く。複数行のブロックは 1 エントリなので Ctrl-P 1 回で丸ごと戻る。読めない履歴 (GNU readline のもの等) には書かないので、書き潰さない                                                                    |
| 裸の式        | `#eval` に包む。評価できない項 (`Real.pi` など) は `#check` に落ちる                                                                                                                                          |
| 単体の `do`   | 最初の action でモナドが決まるのを避け、失敗したら `IO` として読み直す                                                                                                                                        |
| 宣言          | コマンドとして送る。エラーなら環境を進めない (GHCi と同じ)                                                                                                                                                    |
| エラー表示    | 該当行とキャレットを添える                                                                                                                                                                                    |
| 補完 (Tab)    | 定数名を prefix 検索。名前空間ごとにまとめて取ってキャッシュするので、mathlib でも同じ名前空間の 2 回目以降は待ちが無い (実測 1.05 秒 → 0.00 秒)。REPL で通した宣言も候補に入る                              |
| 証明モード    | `sorry` を出したら `:prove <n>` でタクティクを 1 行ずつ試せる。閉じたらその `sorry` だけを台本で埋め戻して通し直すので、複数あれば 1 個ずつ埋めていける。`exact?` `simp?` は提案された項に置き換えて台本に入れる (`:save` したファイルで再検索させないため) |
| init ファイル | `~/.config/leani/init.lean`。base に重ねるので `:reset` しても残る。`:l` は環境を作り直すが、そのあとに重ね直す                                                                                                                                              |
| 復帰          | 評価中の Ctrl-C、エンジンの異常終了、PANIC のいずれでもエンジンを作り直し、通した宣言を replay する (実測 1.4 秒でプロンプトに戻る)。証明していた宣言は `sorry` を拾い直して `:prove` で入り直せる。replay で落ちたものは名前を挙げて報告し、通らなかったぶんも途中で止まって流せなかったぶんも控えて次の `:restart` で流し直す (環境に無いものは `:save` の本体には書かず、コメントとして添える。`:reset` と `:l` は控えも捨てて、そう言う)。作り直しにも失敗して環境が無いあいだは、打った行も `:reset` も断って控えを残す (import を直して `:restart`) |

| コマンド               |                                                                                                                         |
|------------------------|-------------------------------------------------------------------------------------------------------------------------|
| `:t <expr>`            | 型 (`#check`)                                                                                                           |
| `:i <name>`            | 型と docstring                                                                                                          |
| `:p <name>`            | 定義 (`#print`)                                                                                                         |
| `:l <file>` / `:r`     | 読み込み / 読み直し                                                                                                     |
| `:reset` / `:undo [n]` | 環境の操作                                                                                                              |
| `:env [name]`          | 今の環境 / 設定した環境に切り替えて再起動。打った宣言は控えているぶんも含めて切り替え先で流し直す。起動できなければ元の環境に戻る              |
| `:prove [n]`           | `sorry` の証明モードに入る (`:goals` `:script` `:undo` `:done`)。`:goals` は証明モードの外でも残っている `sorry` を出す |
| `:save <file>`         | 通した宣言を `.lean` に書き出す (init と `:l` したぶんも入る)。`:l` でも `lean` でも読める (`:l` したファイルの `import` も書く)。既にあるファイルは断る |
| `:time`                | 実行時間の表示を切り替え                                                                                                |
| `:restart`             | エンジンを作り直して宣言を replay                                                                                       |
| `:{ ... :}`            | 複数行を明示的に囲む                                                                                                    |
| `:!<cmd>`              | shell (Ctrl-C で止めても REPL は続く)                                                                                                                   |
| `:q` / `:help`         | 終了 / 一覧。`leani -h` に CLI 側の一覧、`leani -V` に置き場所                                                          |

証明モードの例:

```
λ> theorem tt (n : Nat) : n + 0 = n := by sorry
sorry 1 [proofState 0]
  n : Nat
  ⊢ n + 0 = n
-- :prove で証明モードに入る (sorry 1 個)
λ> :prove
⊢> induction n with
  | | zero => rfl
  | | succ k ih => simp
  |
証明完了。
-- 埋め戻して通す:
  theorem tt (n : Nat) : n + 0 = n := by
    induction n with
    | zero => rfl
    | succ k ih => simp
λ> #print axioms tt
'tt' depends on axioms: [propext]
```

起動は Lean 本体だけなら 1 秒、中規模の環境で 1.4 秒、mathlib 入りで 6〜11 秒。
olean をどれだけ OS がキャッシュしているかで変わる。

## 置き場所

|                         |                                                     |
|-------------------------|-----------------------------------------------------|
| 設定                    | `~/.config/leani/config.toml` (`LEANI_CONFIG`)      |
| init                    | `~/.config/leani/init.lean` (`LEANI_INIT`)          |
| 履歴                    | `~/.local/state/leani/history` (`LEANI_HISTORY`)    |
| `lake env` のキャッシュ | `~/.local/state/leani/lake-env/`                    |
| エンジン                | `~/.local/state/leani/engine/<版>` (`LEANI_ENGINE`) |

`XDG_CONFIG_HOME` / `XDG_STATE_HOME` があればそちらを見る。`leani -V` で実際に
使っている場所が出る。


## 中身を読むとき

| ファイル           | 中身                                                                          |
|--------------------|-------------------------------------------------------------------------------|
| `SPEC.md`          | 何ができて、どういう性質を持つかの一覧。テストから生成する (`tools/spec.py`)   |
| `tests/stories.py` | 誰の何を助けるか。コードから導けないので、ここが出所                          |
| `REJECTED.md`      | 試して採用しなかった案。同じ案をもう一度思い付いたとき用                      |
| `src/leani.py`     | なぜこのコードがこうなのか。docstring とコメントに書いてある                  |

`src/leani.py` 1 枚。副作用の有無で節を分けて、見出しに札を付けてある。

| 札       | 意味                                                     |
|----------|----------------------------------------------------------|
| 純粋     | 入力だけで出力が決まる。同じ入力なら常に同じ結果         |
| 読み取り | ファイルや環境変数を見るが、何も書き換えない             |
| 副作用   | プロセス・端末・ファイルを触る。上から下へ流れとして読む |
| 定数     | エンジンに投げるクエリの文字列。触るものは無い           |

判定と整形はすべて純粋な側にある (完結判定・エラー位置の枠・提案の取り出し・
補完の単位・設定の検査)。状態を持つのは repl プロセスを抱える `Engine` と、
入力バッファと証明モードを持つ `Repl` の 2 つだけ。分岐は `if` の連続ではなく
`match` / `case` で書き、`return` のあとも `else` を省かないので、`ruff` の
RET505 だけ外してある。

## テスト

```
uv sync
uv run pytest                         # 162 件、2 分半
uv run pytest tests/test_parsing.py   # 純関数だけなら 0.2 秒
uv run pytest -k 履歴                 # 名前で絞る
uv run ruff format . && uv run ruff check .
```

Lean 本体だけを import する環境で走るので、Lake プロジェクトは要らない
(エンジンは初回に自分で用意する)。純関数・エンジン・端末の 3 層で、上ほど速い。
件数はエンジンの層が一番多く (状態の並びで出るバグを追うため)、端末の層は絞って
ある。層の分け方と、エンジン層で 1 行ごとに確認している不変条件は
`tests/conftest.py` の先頭と `INVARIANTS` にある。

テストには `@story(...)` でユーザーストーリーの番号が付いていて、`SPEC.md` は
そこから組む。テストが 0 件のストーリーは `SPEC.md` に空欄として出る (いまは無い。
機能そのものが無い 2 件だけ「まだ作っていない」と出る)。
