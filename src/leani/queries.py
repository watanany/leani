"""エンジンに送るクエリ (定数)。

Lean のソースコードとしてエンジンに送る文字列。操作するものは無い。"""

from __future__ import annotations

import re

PARSE_PROBE = r"""open Lean Parser in
#eval show CoreM Unit from do
  let src := %s
  let env ← getEnv
  let probe : Name → String → Json := fun cat s =>
    match runParserCategory env cat s with
    | .ok _ => Json.mkObj [("ok", Json.bool true)]
    | .error e => Json.mkObj [("ok", Json.bool false), ("err", Json.str e)]
  let tac := "by\n" ++ "\n".intercalate ((src.splitOn "\n").map ("  " ++ ·))
  IO.println (Json.mkObj [("cmd", probe `command src), ("term", probe `term src),
                          ("tac", probe `term tac)]).compress"""

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

INCOMPLETE = re.compile(
    r"unexpected end of input"
    r"|unterminated (comment|string|raw string literal|identifier escape)"
)
META_LINE = re.compile(r"^:[A-Za-z!?{}]")
BLOCK_OPEN = re.compile(r"(?:^|[\s)\]}])(where|with|do|by)[ \t]*$")
ERR_POS = re.compile(r"<input>:(\d+):(\d+):")
DECL_NAME = re.compile(
    r"^\s*(?:@\[[^\]]*\]\s*)*"
    r"(?:private\s+|protected\s+|noncomputable\s+|partial\s+|unsafe\s+|scoped\s+)*"
    r"(?:def|abbrev|theorem|lemma|instance|structure|inductive|class|opaque|axiom)"
    r"\s+([A-Za-z_\u00c0-\uffff][^\s:({\[]*)",
    re.MULTILINE,
)

NONCOMPUTABLE = re.compile(r"noncomputable|failed to compile", re.IGNORECASE)
NO_REPR = re.compile(
    r"could not synthesize.*(Repr|ToString|ToExpr|Eval)", re.DOTALL | re.IGNORECASE
)
CANNOT_EVAL = re.compile(r"cannot evaluate", re.IGNORECASE)

NESTED_ACTION = re.compile(r"must be nested inside a `do` expression")
