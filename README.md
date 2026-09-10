# leani

Lean 4 の対話 REPL。GHCi や ipython のつもりで使える。

エンジンは leanprover-community/repl。あれは JSON in / JSON out の機械向け
プロトコルしか持たないので、ここが人間向けの層を足している。

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

## 入れる

エンジンを clone してビルドしておく。REPL 側に実行時の依存は無いので、
`bin/leani` に PATH を通せばそれで動く。

```
git clone https://github.com/leanprover-community/repl ~/sanctum/projects/lean-repl
cd ~/sanctum/projects/lean-repl && lake build repl

export PATH="$HOME/sanctum/projects/leani/bin:$PATH"
```

エンジンの場所は設定の `engine` か `LEANI_ENGINE` で変えられる。エンジンと
起動する Lake プロジェクトの toolchain がずれていると olean が読めないので、
違っていたら起動時に警告する。

## 環境を選ぶ

「どの Lake プロジェクトの上で何を import して起動するか」を
`~/.config/leani/config.toml` に置く。ghci の `~/.ghci`、ipython の profile と
同じ位置づけで、プロジェクトの名前はコードではなくここにだけ書く。

```toml
default = "global"
engine = "~/sanctum/projects/lean-repl"   # 省略可

[env.global]
project = "~/sanctum/projects/lean-global"
imports = ["Global"]

[env.math]
project = "~/sanctum/projects/lean-global"
imports = ["GlobalMath"]
prompt = "λ∀> "
```

```
leani                  # default の環境
leani -e math          # 名前で選ぶ
leani -p . -i MyLib    # 設定を使わずその場で指定
leani foo.lean         # 読み込んで起動
```

設定が無くても動く。cwd から `lakefile.toml` / `lakefile.lean` を持つ一番近い
親を探し、そこの `lean_lib` を import する。Lake プロジェクトの外なら Lean 本体
だけで起動する。`LEAN_PATH` は `lake env` を呼んで解決し、`lake-manifest.json`
より新しいキャッシュがあれば使い回す (`lake env` は 1 秒近くかかる)。

`import Lean` は設定に関わらず必ず入る。下に書く完結判定と補完のクエリが
`Lean.Parser` と `CoreM` を使うため。

## 入力が終わったかを Lean のパーサに聞く

ここが一番効いている。正規表現で「宣言のキーワードで始まるか」「括弧が閉じたか」を
当てにいくと、想定外の構文が来るたびに早く確定しすぎるか次の行を飲み込む。
代わりに 1 行ごとに `Parser.runParserCategory` を現在の環境で走らせて、
`command` / `term` / `tacticSeq` として読めるかを聞く (実行はしないので副作用も
無く、ユーザ定義の notation も効く。1 往復 60ms 程度)。

* 読めた → その種類で送る。`term` なら `#eval` に包む。
* `unexpected end of input` → 途中なので次の行を待つ。
* どちらでもない → そのまま送って Lean 本体にエラーを出させる。自前の推測で
  エラーを作らない。

ただしパーサだけでは足りない。Lean では `structure P where` も `def f := 1` も
`induction n with` も、それ自体で完結した構文として通る。にもかかわらず次の
インデント行は続きになりうる。そこで:

* インデントの続く限り読み、空行かインデントの切れた行で確定する (Python の
  REPL と同じ)。行頭の `|` も継続扱いにする (`|` で始まる command も tactic も
  Lean には無いので、必ず前の行の続き)。
* `where` `with` `do` `by` で終わる行は、完結していても空行を待つ。
* それでも確定してしまった直後にインデント行が来たら、**直前の入力に遡って
  続きとして読み直す**。環境も 1 つ戻すので、`structure P where` を送った後に
  フィールドを書き足せる。

## 落ちても続く

| 事象 | 挙動 |
| --- | --- |
| 評価中に Ctrl-C | エンジンを作り直して、通した宣言を replay する。実測 1.4 秒でプロンプトに戻る |
| エンジンが落ちた | 同じ経路で復帰し、失敗したリクエストを 1 回だけやり直す |
| エンジンが PANIC | 結果扱いせず `:restart` を促す |

replay の対象は「エラー無く通った宣言」だけ。`#eval` は環境に残らないので記録しない。

## できること

| 機能 | 中身 |
| --- | --- |
| 行編集・履歴 | readline (macOS は libedit)。履歴は `~/.local/state/leani/history` に毎行書く。複数行のブロックは 1 エントリなので Ctrl-P 1 回で丸ごと戻る |
| 裸の式 | `#eval` に包む。評価できない項 (`Real.pi` など) は `#check` に落ちる |
| 単体の `do` | 最初の action でモナドが決まるのを避け、失敗したら `IO` として読み直す |
| 宣言 | コマンドとして送る。エラーなら環境を進めない (GHCi と同じ) |
| エラー表示 | 該当行とキャレットを添える |
| 補完 (Tab) | 定数名を prefix 検索。名前空間ごとにまとめて取ってキャッシュするので、mathlib でも同じ名前空間の 2 回目以降は待ちが無い (実測 1.05 秒 → 0.00 秒)。REPL で通した宣言も候補に入る |
| 証明モード | `sorry` を出したら `:prove` でタクティクを 1 行ずつ試せる。閉じたら `by sorry` を台本で埋め戻して通し直す。`exact?` `simp?` は提案された項に置き換えて台本に入れる (`:save` したファイルで再検索させないため) |
| init ファイル | `~/.config/leani/init.lean`。base に重ねるので `:reset` しても残る |

| コマンド | |
| --- | --- |
| `:t <expr>` | 型 (`#check`) |
| `:i <name>` | 型と docstring |
| `:p <name>` | 定義 (`#print`) |
| `:l <file>` / `:r` | 読み込み / 読み直し |
| `:reset` / `:undo [n]` | 環境の操作 |
| `:env [name]` | 今の環境 / 設定した環境に切り替えて再起動 |
| `:prove [n]` | `sorry` の証明モードに入る (`:goals` `:script` `:undo` `:done`)。`:goals` は証明モードの外でも残っている `sorry` を出す |
| `:save <file>` | 通した宣言を `.lean` に書き出す。`:l` でも `lean` でも読める |
| `:time` | 実行時間の表示を切り替え |
| `:restart` | エンジンを作り直して宣言を replay |
| `:{ ... :}` | 複数行を明示的に囲む |
| `:!<cmd>` | shell |
| `:q` / `:help` | 終了 / 一覧。`leani -h` に CLI 側の一覧、`leani -V` に置き場所 |

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

## 環境の pickle を使っていない理由

repl には `pickleTo` / `unpickleEnvFrom` があり、起動短縮に使えそうに見える。
pickle は import からの差分しか持たない (1.2KB 程度) ので unpickle でも olean の
読み込みは同じだけ走り、実測で import 1.3 秒 / unpickle 1.2 秒、mathlib は
5.3 秒前後で差が無い。さらに戻した環境で `#eval` すると Lean のコンパイラが
PANIC する。セッションの保存は `:save` でソースとして残すほうが確実で、編集も
`lean` での実行もできる。

## テスト

pytest。依存は uv で入れる。

```
uv sync
uv run pytest              # 45 秒前後
uv run pytest -k 履歴      # 名前で絞る
```

Lean 本体だけを import する環境で走るので、Lake プロジェクトは要らない
(エンジンのビルドだけ要る)。下ほど速く、下ほど数を多く持つ 3 層に分けてある。

| ファイル | 相手 | 1 件あたり |
| --- | --- | --- |
| `tests/test_parsing.py` | 純関数だけ (継続判定、提案の取り出し、補完の単位) | ミリ秒 |
| `tests/test_engine.py` | 本物のエンジン。`Repl.feed()` を直接叩く | 1.5 秒 |
| `tests/test_completion.py` | 同上。問い合わせ回数は `mocker.spy` で数える | 1.5 秒 |
| `tests/test_terminal.py` | pty 越しの本物の readline。端末が絡むものだけ | 1.5 秒 |

エンジン層では 1 行食わせるごとに不変条件を全部確認する
(`tests/conftest.py` の `INVARIANTS`)。

* env のスタックと宣言ログの長さが一致する
* 環境 id を持っている
* 持ち越した proofState は現在の環境のもの
* 証明の台本と巻き戻し用スタックの長さが一致する

この REPL は env のスタックと証明モードを持つ状態機械で、踏んだバグはどれも
単発の操作ではなく操作の並びで出た。個別の assert とは別に「どの状態でも
成り立つはずのこと」を毎回見ておくと、想定していない並びでも捕まる。実際
2 つめの不変条件が「読み込みに失敗すると環境を失う」を見つけた。

## 置き場所

| | |
| --- | --- |
| 設定 | `~/.config/leani/config.toml` (`LEANI_CONFIG`) |
| init | `~/.config/leani/init.lean` (`LEANI_INIT`) |
| 履歴 | `~/.local/state/leani/history` (`LEANI_HISTORY`) |
| `lake env` のキャッシュ | `~/.local/state/leani/lake-env/` |
| エンジン | `~/sanctum/projects/lean-repl` (`LEANI_ENGINE`) |

`XDG_CONFIG_HOME` / `XDG_STATE_HOME` があればそちらを見る。`leani -V` で実際に
使っている場所が出る。
