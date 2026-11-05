# Proof mode

A mode for entering tactics one at a time while looking at the goals, as you do when you write Lean in an editor.

The REPL does not accept an unfinished proof, so write `sorry` where you want to fill in the proof and enter the theorem.
leani shows the goal at the position of `sorry`. Then run `:prove`,
and you can fill that `sorry` with tactics one at a time.
When the proof is finished, leani runs the theorem again with `sorry` replaced by the proof.

## Example

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

## Where to write sorry

`sorry` may be written in the middle of a proof. The goal shown is the goal after running the tactics
up to that position. Only `sorry` is replaced, and the tactics before and after it stay as they are.

```lean
theorem foo (hp : p) (hq : q) : p ∧ q := by
  constructor
  · exact hp
  · sorry
```

When there are two or more `sorry`, specify which `sorry` by its number, as in `:prove 2`.
After you finish proving one, you can prove the remaining `sorry` with `:prove <n>` in turn.
A `sorry` written in term position, as in `:= sorry`, cannot be replaced (the declaration stays as `sorry`).

## Input

- One input is one tactic.
- When you enter a line that needs a continuation, such as `induction n with`, leani waits for the continuation with ` | `. An empty line finishes the input.
- When there are two or more goals, leani shows all of them with numbers, as in `goal 1/2`.
- A tactic that results in an error is not recorded. The state does not change, so you can enter a different tactic.

## Commands

| Command        | Action                                                  |
|----------------|---------------------------------------------------------|
| `:goals`, `:g` | Shows the current goals                                 |
| `:script`      | Shows the tactics entered so far                        |
| `:undo`        | Undoes the last tactic                                  |
| `:done`, `:q`  | Stops the proof partway (the declaration stays `sorry`) |
| `:help`        | Shows the proof mode commands                           |

Other commands such as `:t` and `:i` can be used while in proof mode.
In proof mode, `:q` does not quit leani. It only ends proof mode.

When you run `:goals` outside proof mode, leani shows the remaining `sorry` and the goal of each.

## Suggested tactics

When you enter `exact?` or `simp?`, leani records the tactic that Lean suggests (such as `exact Nat.add_zero n`) in the script.
The finished declaration contains the suggested tactic, not `exact?`.

## Notes

- `:prove` can only prove the `sorry` in **the declaration entered last**. If you enter another declaration after it, `:prove` can no longer be used.
- If the engine restarts during a proof, proof mode ends. The tactics you entered are not applied.
