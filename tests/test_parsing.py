"""入力の読み方を決めている純関数。端末もエンジンも要らないのでミリ秒で終わる。"""
from conftest import leani


def describe_継続行の判定():
    """単独では入力になりえない行を見分ける。"""

    def it_インデントした行は前の行の続き():
        for line in ("  | 0 => 0", "\tfoo", "  bar"):
            assert leani.continues(line), f"継続にならない: {line!r}"

    def it_行頭のパイプは前の行の続き():
        # `|` で始まる command も tactic も Lean には無い。
        assert leani.continues("| zero => rfl")

    def it_ふつうの行は単独の入力():
        for line in ("def f := 1", "#eval 1", "", "x + 1"):
            assert not leani.continues(line), f"継続にされた: {line!r}"


def describe_続きを待つ行():
    """完結していても次の行がありうる構文。"""

    def it_where_with_do_by_で終わる行は空行を待つ():
        for line in ("structure P where", "induction n with",
                     "def f := do", "theorem t : True := by"):
            assert leani.BLOCK_OPEN.search(line), f"待たない: {line!r}"

    def it_語の一部として含むだけなら待たない():
        for line in ("def where_ := 1", "#eval byte", "def f := 1"):
            assert not leani.BLOCK_OPEN.search(line), f"余計に待つ: {line!r}"


def describe_パーサからの返事():
    """「まだ途中」だけを継続と読む。"""

    def it_入力の途中なら次の行を待つ():
        assert leani.INCOMPLETE.search("<input>:1:5: unexpected end of input")
        assert leani.INCOMPLETE.search("unterminated comment")

    def it_ただのエラーは待たずに送る():
        assert not leani.INCOMPLETE.search("unknown identifier 'foo'")


def describe_タクティクの提案():
    """exact? や simp? は結果を "Try this:" として返す。"""

    def it_提案から項だけを取り出す():
        found = leani.try_this([
            {"severity": "info", "data": "Try this:\n  [apply] exact Nat.le_refl n"}
        ])
        assert found == "exact Nat.le_refl n"

    def it_提案でなければ何も返さない():
        assert leani.try_this([{"severity": "info", "data": "goals accomplished"}]) is None
        assert leani.try_this([]) is None


def describe_宣言の名前拾い():
    """自分で通した宣言を補完の候補に足すために使う。"""

    def it_修飾子や属性が付いていても名前を取れる():
        cases = {
            "def foo := 1": "foo",
            "theorem bar (n : Nat) : n = n := rfl": "bar",
            "@[simp] private noncomputable def baz : Nat := 0": "baz",
            "structure Point where\n  x : Nat": "Point",
        }
        for src, want in cases.items():
            assert leani.DECL_NAME.findall(src) == [want], f"取れない: {src!r}"

    def it_宣言でないものは拾わない():
        assert leani.DECL_NAME.findall("#eval 1 + 1") == []


def describe_補完をまとめて取る単位():
    """名前空間があればそこまで、無ければ先頭 2 文字。"""

    def it_名前空間で切る():
        assert leani.Repl._chunk("Nat.suc") == "Nat."
        assert leani.Repl._chunk("MeasureTheory.integral_") == "MeasureTheory."

    def it_名前空間が無ければ二文字():
        # mathlib では 1 文字だと `C` で 7.5 万件になるので広げすぎない。
        assert leani.Repl._chunk("Contin") == "Co"
