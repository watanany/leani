"""エンジンに送るクエリ (定数)。

Lean のソースコードとしてエンジンに送る文字列。操作するものは無い。"""

from __future__ import annotations

import re

# 完結判定。command / term / tacticSeq としてパースできるかを、1 回のリクエストで
# 確かめる。
PARSE_PROBE = r"""open Lean Parser in
#eval show CoreM Unit from do
  let src := %s
  let env ← getEnv
  let probe : Name → Json := fun cat =>
    match runParserCategory env cat src with
    | .ok _ => Json.mkObj [("ok", Json.bool true)]
    | .error e => Json.mkObj [("ok", Json.bool false), ("err", Json.str e)]
  IO.println (Json.mkObj [("cmd", probe `command), ("term", probe `term),
                          ("tac", probe `tacticSeq)]).compress"""

# 補完の候補。複数の接頭辞の候補を、定数を 1 回走査するあいだにまとめて取得する
# (Mathlib では 1 回の走査に 1 秒かかる)。protected な名前には先頭に ! を付ける。
# `open` した名前空間の名前を短い名前で書けるかどうかは、この ! で決まる。
COMPLETE_QUERY = r"""open Lean in
#eval show CoreM Unit from do
  let env ← getEnv
  let keys : Array String := %s
  let mut hits : Array (Array (String × Bool)) := keys.map fun _ => #[]
  for (n, _) in env.constants.toList do
    if n.isInternalDetail then continue
    let last := n.getString!
    if last.startsWith "_" || last.startsWith "eq_" || last.startsWith "match_"
       || last.startsWith "proof_" || last.startsWith "congr_"
       || last == "go" || last == "loop" || last == "induct" || last == "fun_cases"
       || last == "eq_def" || last == "sizeOf_spec" then continue
    let s := n.toString
    for i in [0:keys.size] do
      if keys[i]!.isPrefixOf s then
        hits := hits.modify i (·.push (s, isProtected env n))
  let mark := fun ((s, p) : String × Bool) => if p then "!" ++ s else s
  let out := hits.map fun h => (h.qsort (·.1 < ·.1)).toList.take %d |>.map mark
  IO.println (toJson out).compress"""

# 今の namespace と open。短い名前がどの名前空間の名前かを判断するのに使う。
# `open Lean in` を付けるとその open 自体も結果に含まれるので、名前はすべて
# 完全修飾名で書く。
SCOPE_QUERY = r"""#eval show Lean.CoreM Unit from do
  let mut opens : Array Lean.Json := #[]
  let mut ns ← Lean.getCurrNamespace
  while !ns.isAnonymous do
    opens := opens.push (Lean.toJson (ns.toString, ([] : List String)))
    ns := ns.getPrefix
  let mut aliases : Array Lean.Json := #[]
  for d in ← Lean.getOpenDecls do
    match d with
    | .simple n ex =>
      opens := opens.push (Lean.toJson (n.toString, ex.map (·.toString)))
    | .explicit id decl =>
      aliases := aliases.push (Lean.toJson (id.toString, decl.toString))
  let out := Lean.Json.mkObj [("open", .arr opens), ("alias", .arr aliases)]
  IO.println out.compress"""

DOC_QUERY = r"""open Lean in
#eval show CoreM Unit from do
  match ← findDocString? (← getEnv) `%s with
  | some d => IO.println d
  | none => pure ()"""

# パーサが「まだ続きがある」と報告しているとみなすメッセージ。
INCOMPLETE = re.compile(r"unexpected end of input|unterminated (comment|string)")
# 複数行の入力の途中でも、: で始まる行でブロックを終わらせられるようにする (ブロックが
# 完結していれば送信し、途中なら捨てる)。Lean のソースの行が : で始まることは
# 無い (`:=` の続きの行はインデントされる)。
META_LINE = re.compile(r"^:[A-Za-z!?{}]")
# `structure P where` や `induction n with` は Lean の文法ではそれだけで完結する。
# パーサは完結していると判定するが、普通は続きのブロックを書きたいので確定を遅らせる。
# 完結判定の結果を変えるわけではないので、推測が外れても Enter が 1 回余分に必要に
# なるだけ。
BLOCK_OPEN = re.compile(r"(?:^|[\s)\]}])(where|with|do|by)[ \t]*$")
ERR_POS = re.compile(r"<input>:(\d+):(\d+):")
# ユーザーが入力し、エラーなく受理された宣言の名前。補完の候補に追加するためだけに
# 使うので、取りこぼしても問題はない。
DECL_NAME = re.compile(
    r"^\s*(?:@\[[^\]]*\]\s*)*"
    r"(?:private\s+|protected\s+|noncomputable\s+|partial\s+|unsafe\s+|scoped\s+)*"
    r"(?:def|abbrev|theorem|lemma|instance|structure|inductive|class|opaque|axiom)"
    r"\s+([A-Za-z_\u00c0-\uffff][^\s:({\[]*)",
    re.MULTILINE,
)

# #eval できない式。型だけでも表示したほうが親切なので、#check に切り替える。
# 評価できない理由を表示するため、理由ごとに分ける。
NONCOMPUTABLE = re.compile(r"noncomputable|failed to compile", re.IGNORECASE)
NO_REPR = re.compile(
    r"could not synthesize.*(Repr|ToString|ToExpr|Eval)", re.DOTALL | re.IGNORECASE
)
CANNOT_EVAL = re.compile(r"cannot evaluate", re.IGNORECASE)
