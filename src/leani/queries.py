"""エンジンに投げるクエリ (定数)。

Lean のソースとしてエンジンに送る文字列。触るものは無い。"""

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

COMPLETE_QUERY = r"""open Lean in
#eval show CoreM Unit from do
  let env ← getEnv
  let mut ns : Array String := #[]
  for (n, _) in env.constants.toList do
    if n.isInternalDetail then continue
    let last := n.getString!
    if last.startsWith "_" || last.startsWith "eq_" || last.startsWith "match_"
       || last.startsWith "proof_" || last.startsWith "congr_"
       || last == "go" || last == "loop" || last == "induct" || last == "fun_cases"
       || last == "eq_def" || last == "sizeOf_spec" then continue
    let s := n.toString
    if %s.isPrefixOf s then ns := ns.push s
  IO.println (String.intercalate " " (ns.qsort.toList.take %d))"""

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
# 完結判定を覆すわけではないので、外しても Enter が 1 回余分に要るだけで済む。
BLOCK_OPEN = re.compile(r"(?:^|[\s)\]}])(where|with|do|by)[ \t]*$")
ERR_POS = re.compile(r"<input>:(\d+):(\d+):")
# 自分で通した宣言の名前。補完に足すためだけなので、取りこぼしても害はない。
DECL_NAME = re.compile(
    r"^\s*(?:@\[[^\]]*\]\s*)*"
    r"(?:private\s+|protected\s+|noncomputable\s+|partial\s+|unsafe\s+|scoped\s+)*"
    r"(?:def|abbrev|theorem|lemma|instance|structure|inductive|class|opaque|axiom)"
    r"\s+([A-Za-z_\u00c0-\uffff][^\s:({\[]*)",
    re.M,
)

# #eval できない式。型だけでも出したほうが親切なので #check に落とす。
NOT_EVALUABLE = re.compile(
    r"noncomputable|failed to compile"
    r"|could not synthesize.*(Repr|ToString|ToExpr|Eval)|cannot evaluate",
    re.S | re.I,
)
