# 使い方

コマンドの一覧は REPL の中で `:help` を、起動オプションは `leani -h` を実行すると表示される。

## 証明モード

`sorry` のある定理を書いた直後に `:prove` を実行すると、タクティクを 1 つずつ試せる。
証明が終わると、leani は `sorry` を証明に置き換えた宣言を実行し直す。

```
λ> theorem tt (n : Nat) : n + 0 = n := by sorry
1:9  theorem tt (n : Nat) : n + 0 = n := by sorry
             ^^
  declaration uses `sorry`
sorry 1 [proofState 0]
  n : Nat
  ⊢ n + 0 = n
-- :prove で証明モードを始められる (sorry 1 個)
λ> :prove
証明モード: 1 回の入力が 1 タクティク。:goals ゴール  :script スクリプト  :undo 取り消す  :done 終了
goal
  n : Nat
  ⊢ n + 0 = n
⊢> induction n with
 |   | zero => rfl
 |   | succ k ih => simp
 |
証明完了。
-- スクリプト:
  induction n with
    | zero => rfl
    | succ k ih => simp
-- sorry をスクリプトで置き換えて実行する:
  theorem tt (n : Nat) : n + 0 = n := by
    induction n with
      | zero => rfl
      | succ k ih => simp
```

`exact?` や `simp?` を使うと、Lean が提案したタクティクがスクリプトに残る。

## 評価を止めたとき

Ctrl-C で評価を止めたり、エンジンが落ちたりしても、leani はエンジンを再起動して、それまでの宣言を実行し直す。

## うまくいかないとき

ほとんどのエラーは、メッセージに直し方が表示される。
次の 2 つだけは、メッセージだけでは分かりにくい。

- **`import に失敗した`**。leani はプロジェクトをビルドしないので、Lake プロジェクトで先に `lake build` を実行する。Mathlib を使う場合は `lake exe cache get` を実行する
- **`elan が PATH に無い`、`lake が PATH に無い`**。elan をインストールして、`~/.elan/bin` を PATH に追加する
