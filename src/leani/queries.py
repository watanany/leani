"""エンジンに投げるクエリ (定数)。

Lean のソースとしてエンジンに送る文字列。操作するものは無い。"""

from __future__ import annotations

import re

# 完結判定。command と term の両方を 1 往復で聞く。
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

# 補完の候補。名前空間ごとの塊をいくつか、定数を 1 周するあいだにまとめて取る
# (mathlib では 1 周に 1 秒かかる)。protected な名前は先頭に ! を付ける。
# `open` した先から短い名前で書けるかどうかがこれで決まる。
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

# いまの namespace と open。短い名前がどの名前空間から来るかを決める。
# `open Lean in` を付けると、それ自体が答えに混ざるので名前は全部修飾して書く。
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

# パーサが「まだ続きがある」と言っているとみなすメッセージ。
INCOMPLETE = re.compile(r"unexpected end of input|unterminated (comment|string)")
# ブロック中でも脱出できるようにする。Lean のソースが行頭 : で始まることは無い
# (`:=` の継続はインデントされる)。
META_LINE = re.compile(r"^:[A-Za-z!?{}]")
# `structure P where` や `induction n with` は Lean 文法ではそれ自体で完結する。
# パーサは「終わり」と言うが、続きのブロックを書きたいのが普通なので確定を遅らせる。
# 完結判定を覆すわけではないので、外しても Enter が 1 回余分に必要になるだけ。
BLOCK_OPEN = re.compile(r"(?:^|[\s)\]}])(where|with|do|by)[ \t]*$")
ERR_POS = re.compile(r"<input>:(\d+):(\d+):")
# 自分で通した宣言の名前。補完に足すためだけなので、取りこぼしても害はない。
DECL_NAME = re.compile(
    r"^\s*(?:@\[[^\]]*\]\s*)*"
    r"(?:private\s+|protected\s+|noncomputable\s+|partial\s+|unsafe\s+|scoped\s+)*"
    r"(?:def|abbrev|theorem|lemma|instance|structure|inductive|class|opaque|axiom)"
    r"\s+([A-Za-z_\u00c0-\uffff][^\s:({\[]*)",
    re.MULTILINE,
)

# #eval できない式。型だけでも出したほうが親切なので #check に落とす。
NOT_EVALUABLE = re.compile(
    r"noncomputable|failed to compile"
    r"|could not synthesize.*(Repr|ToString|ToExpr|Eval)|cannot evaluate",
    re.DOTALL | re.IGNORECASE,
)
