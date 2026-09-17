"""略記表と展開 (純粋)。

`\\to` を `→` にする表。VS Code の Lean 拡張と同じ綴りを使う。"""

from __future__ import annotations

from typing import Final

# 略記表。VS Code の Lean 拡張が持つ 1857 件から、よく打つものだけを引いた。
# 丸ごと抱えると 25 KB のデータでこのファイルが埋まるうえ、鍵の 2 割が他の鍵の
# 接頭辞なので、確定の仕方まで作り込まないと持て余す。値は本家のものをそのまま
# 使うので、`\d` が δ ではなく ↓ のような意外な対応もそのままにしてある。
#   https://github.com/leanprover/vscode-lean4/blob/master/lean4-unicode-input/src/abbreviations.json
# fmt: off
ABBREV: Final[dict[str, str]] = {
    # ギリシャ文字
    "a": "α", "alpha": "α", "b": "β", "beta": "β", "g": "γ", "gamma": "γ",
    "delta": "δ", "e": "ε", "eps": "ε", "epsilon": "ε", "zeta": "ζ",
    "eta": "η", "th": "θ", "theta": "θ", "iota": "ι", "kappa": "κ",
    "lam": "λ", "lambda": "λ", "fun": "λ", "mu": "μ", "nu": "ν", "xi": "ξ",
    "pi": "π", "rho": "ρ", "si": "σ", "sigma": "σ", "tau": "τ",
    "upsilon": "υ", "phi": "φ", "varphi": "ϕ", "chi": "χ", "psi": "ψ",
    "omega": "ω",
    "G": "Γ", "Gamma": "Γ", "D": "Δ", "Delta": "Δ", "Theta": "Θ", "L": "Λ",
    "Lambda": "Λ", "Xi": "Ξ", "P": "Π", "Pi": "Π", "S": "Σ", "Sigma": "Σ",
    "Phi": "Φ", "Psi": "Ψ", "Omega": "Ω",
    # 矢印
    "l": "←", "leftarrow": "←", "r": "→", "to": "→", "imp": "→",
    "rightarrow": "→", "u": "↑", "uparrow": "↑", "d": "↓",
    "downarrow": "↓", "lr": "↔", "iff": "↔", "mapsto": "↦", "hom": "⟶",
    "longrightarrow": "⟶",
    # 論理
    "all": "∀", "forall": "∀", "ex": "∃", "exists": "∃", "nexists": "∄",
    "and": "∧", "or": "∨", "not": "¬", "neg": "¬", "bot": "⊥", "top": "⊤",
    "vdash": "⊢", "entails": "⊢", "models": "⊧", "therefore": "∴",
    "because": "∵", "qed": "∎",
    # 集合
    "in": "∈", "nin": "∉", "sub": "⊆", "subset": "⊆", "subseteq": "⊆",
    "ssub": "⊂", "supset": "⊇", "supseteq": "⊇", "cup": "∪", "union": "∪",
    "cap": "∩", "inter": "∩", "bigcup": "⋃", "Union": "⋃", "bigcap": "⋂",
    "Inter": "⋂", "empty": "∅", "emptyset": "∅", "smallsetminus": "∖",
    "compl": "ᶜ", "sup": "⊔", "Sup": "⨆", "inf": "⊓", "Inf": "⨅",
    # 関係
    "le": "≤", "ge": "≥", "ne": "≠", "sim": "∼", "simeq": "≃",
    "equiv": "≃", "cong": "≅", "approx": "≈", "ll": "≪", "gg": "≫",
    "dvd": "∣", "mid": "∣", "parallel": "∥", "perp": "⟂",
    # 型
    "N": "ℕ", "nat": "ℕ", "Z": "ℤ", "int": "ℤ", "Q": "ℚ", "R": "ℝ",
    "real": "ℝ", "C": "ℂ", "aleph": "ℵ", "ell": "ℓ",
    # 演算
    "o": "∘", "circ": "∘", "comp": "∘", "dot": "·", "cdot": "·",
    "t": "▸", "tr": "⬝", "inv": "⁻¹", "times": "×", "div": "÷", "pm": "±",
    "mp": "∓", "otimes": "⊗", "oplus": "⊕", "odot": "⊙", "star": "⋆",
    "sqrt": "√", "infty": "∞", "sum": "∑", "prod": "∏", "integral": "∫",
    "partial": "∂", "nabla": "∇", "prime": "′", "dagger": "†",
    # 括弧と点
    "langle": "⟨", "rangle": "⟩", "lceil": "⌈", "rceil": "⌉",
    "lfloor": "⌊", "rfloor": "⌋", "ldots": "…", "cdots": "⋯",
    "vdots": "⋮", "ddots": "⋱",
    # 添字
    "0": "₀", "1": "₁", "2": "₂", "3": "₃", "4": "₄", "5": "₅", "6": "₆",
    "7": "₇", "8": "₈", "9": "₉", "_1": "₁", "_2": "₂", "_i": "ᵢ",
    "_n": "ₙ", "^1": "¹", "^2": "²", "^3": "³", "^n": "ⁿ", "^-1": "⁻¹",
    "^c": "ᶜ",
}
# fmt: on


def expand_abbrev(head: str) -> tuple[str, int] | None:
    """
    カーソルの手前にある `\\name` を記号にする。(記号, 消す文字数) を返す。

    確定は space に任せて、打鍵ごとには変換しない。`\\a` `\\all` `\\alpha` の
    ように鍵が鍵の接頭辞になっている組が多く、打つそばから確定すると `\\alpha`
    と打てなくなる。space まで待てばどれを打ったのかは一意に決まる。

    表に無ければ何も返さない。`\\` のあとを空白まで取って丸ごと引くので、
    `\\to)` のように記号が続いた形は変換しない (space を先に打つ)。
    """
    cut = head.rfind("\\")
    if cut < 0:
        return None

    name = head[cut + 1 :]
    sym = ABBREV.get(name)
    if sym is None:
        return None

    return sym, len(name) + 1
