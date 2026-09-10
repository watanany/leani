"""Repl を直接叩く層。端末は挟まないが、本物のエンジンと話す。

`repl` fixture は 1 行食わせるごとに不変条件 (conftest.INVARIANTS) を
確認するので、各テストの assert は「その操作で何が起きてほしいか」だけを
書けばいい。
"""

import contextlib
import io

import pytest
from conftest import story

import leani


def describe_式の評価():

    @story("A1")
    def it_裸の式は評価されて値が出る(repl):
        assert "2" in repl.feed("1 + 1")

    @story("A1", "B1")
    def it_IO_の式はそのまま走る(repl):
        assert "hi" in repl.feed('IO.println "hi"')

    @story("A2")
    def it_評価できない項は型だけ出す(repl):
        # Nat -> Nat に Repr が無いので #eval できない。
        out = repl.feed("Nat.succ")
        assert "型だけ" in out, out


def describe_複数行の宣言():

    @story("C1")
    def it_空行までまとめて一つの宣言として通る(repl):
        repl.block(
            "def fib : Nat -> Nat",
            "  | 0 => 0",
            "  | 1 => 1",
            "  | n+2 => fib n + fib (n+1)",
        )
        assert "55" in repl.feed("fib 10")
        assert len(repl.declarations) == 1

    @story("C2")
    def it_確定した直後のインデント行は前の入力に遡って続きになる(repl):
        repl.feed("def three := 1")
        repl.block("  + 2")
        assert "3" in repl.feed("three")
        assert len(repl.declarations) == 1, "遡らずに 2 件になった"

    @story("C1", "F1")
    def it_エラーになった宣言は環境を進めない(repl):
        repl.feed('def broken : Nat := "oops"')
        assert repl.declarations == []


def describe_証明モード():

    @story("E2")
    def it_exact_の提案を台本に入れる(repl):
        # `exact?` のままだと :save したファイルで毎回検索が走り、
        # 結果も環境次第で変わる。
        repl.feed("theorem t1 (n : Nat) : n + 0 = n := by sorry")
        repl.feed(":prove")
        out = repl.feed("exact?")  # 1 手で閉じるのでそのまま抜ける
        assert "台本には" in out, out
        assert "証明完了" in out, out
        assert "exact?" not in out[out.index("証明完了") :], "埋め戻しに残った"

    @story("E1")
    def it_複数手の証明で目標と台本を見られる(repl):
        repl.feed("theorem t2 : 1 = 1 ∧ 2 = 2 := by sorry")
        repl.feed(":prove")
        repl.feed("constructor")
        assert "1 = 1" in repl.feed(":goals")
        assert "constructor" in repl.feed(":script")
        repl.feed("rfl")
        assert "証明完了" in repl.feed("rfl")

    @story("E1")
    def it_証明モードの外で打っても困らせない(repl):
        # 証明が通ると自動で抜けるので、その直後に打たれることがある。
        assert "証明モードではない" in repl.feed(":done")
        assert "証明モードではない" in repl.feed(":goals")

    @story("E1", "F2")
    def it_undo_のあとの_prove_が手前の宣言を消さない(repl):
        repl.feed("def keepme := 42")
        repl.feed("theorem t0 : 1 = 1 := by sorry")
        repl.feed(":undo")
        repl.feed(":prove")
        assert "42" in repl.feed("keepme"), "手前の宣言が巻き戻された"
        assert len(repl.declarations) == 1


def describe_ファイルの読み書き():

    @story("B3", "B4")
    def it_save_したファイルを読み直せる(repl, tmp_path):
        path = tmp_path / "saved.lean"
        repl.feed("def saved := 7")
        assert str(path) in repl.feed(f":save {path}")
        assert "def saved := 7" in path.read_text()
        repl.feed(":reset")
        repl.feed(f":l {path}")
        assert "7" in repl.feed("saved")

    @story("B4", "F1")
    def it_読み込みに失敗しても手元の環境を失わない(repl, tmp_path):
        # env を捨てたまま戻さないと、以後の入力が import 無しの環境に飛んで
        # 何を書いても通らなくなる (不変条件「環境 id を持っている」で発見)。
        broken = tmp_path / "broken.lean"
        broken.write_text('def broken : Nat := "oops"\n')
        repl.feed("def before := 5")
        assert "読み込めなかった" in repl.feed(f":l {broken}")
        assert "5" in repl.feed("before")

    @story("B4")
    def it_読めないファイルは理由を出す(repl, tmp_path, mocker):
        path = tmp_path / "locked.lean"
        path.write_text("def x := 1\n")
        mocker.patch(
            "leani.open",
            create=True,
            side_effect=PermissionError(13, "Permission denied"),
        )
        assert "読めない" in repl.feed(f":l {path}")


def describe_落ちても続く():

    @story("F1")
    def it_エンジンが落ちたら作り直して宣言を_replay_する(repl, mocker):
        repl.feed("def survivor := 9")
        real = repl.engine.send_cmd
        crashed = []

        def crash_once(src, fresh=False):
            if not crashed and src.startswith("def afterCrash"):
                crashed.append(src)
                raise leani.EngineDied()
            return real(src, fresh=fresh)

        mocker.patch.object(repl.engine, "send_cmd", side_effect=crash_once)
        out = repl.feed("def afterCrash := 3")
        assert "作り直す" in out, out
        assert "1 件を replay" in out, out
        assert "9" in repl.feed("survivor"), "replay されていない"
        assert "3" in repl.feed("afterCrash"), "やり直されていない"


def describe_環境の切り替え():

    @story("G5")
    def it_切り替えても設定と読み込んだファイルが残る(repl, tmp_path):
        path = tmp_path / "switched.lean"
        repl.feed("def switched := 11")
        repl.feed(f":save {path}")
        repl.feed(":reset")
        repl.feed(f":l {path}")
        repl.feed(":time")
        repl.feed(":env wide")
        assert repl.repl.cfg.name == "wide"
        assert repl.repl.show_time, ":time の設定が消えた"
        assert "11" in repl.feed("switched"), ":l したファイルが引き継がれていない"

    @story("G5")
    def it_設定に無い名前を言われても続く(repl):
        assert "設定に無い" in repl.feed(":env nosuch")
        assert "2" in repl.feed("1 + 1"), "環境を壊して抜けている"

    @story("G4", "G5")
    def it_切り替え先が起動しなければ元の環境に戻る(repl, mocker):
        # import が通るかは boot するまで分からない。今のエンジンは kill 済み
        # なので、そのまま投げるとセッションごと消える。
        real = leani.Engine.boot

        def only_core(self):
            if self.cfg.name != "core":
                raise leani.EngineDied()
            return real(self)

        mocker.patch.object(leani.Engine, "boot", only_core)
        out = repl.feed(":env wide")
        assert "起動できなかった" in out, out
        assert repl.repl.cfg.name == "core", "元の環境に戻っていない"
        assert "2" in repl.feed("1 + 1"), "使えなくなっている"


def describe_作り直せないとき():
    """boot が通らないこともある。報告して続ける (落ちない)。"""

    @story("F1")
    def it_作り直しに失敗しても報告だけで済む(repl, mocker):
        repl.feed("def kept := 4")
        mocker.patch.object(leani.Engine, "boot", side_effect=leani.EngineDied())
        out = repl.feed(":restart")
        assert "作り直せなかった" in out, out

    @story("F1")
    def it_失敗しても宣言を捨てない(repl, mocker):
        # 捨てると、直してやり直しても replay できなくなる。ただし log には
        # 残せない (env が無いので「環境に入っている宣言」ではない)。
        repl.feed("def kept := 4")
        mocker.patch.object(leani.Engine, "boot", side_effect=leani.EngineDied())
        repl.feed(":restart")
        assert repl.repl.eng.unplayed == ["def kept := 4"], repl.repl.eng.unplayed
        assert repl.repl.eng.log == []

        mocker.stopall()
        assert "1 件を replay" in repl.feed(":restart")
        assert "4" in repl.feed("kept")


def describe_書き出しの安全側():
    """:save は宣言を残す操作。そこで宣言を失わせない。"""

    @story("B3")
    def it_既にあるファイルは上書きしない(repl, tmp_path):
        path = tmp_path / "existing.lean"
        path.write_text("-- 大事なもの\n")
        repl.feed("def x := 1")
        assert "すでにある" in repl.feed(f":save {path}")
        assert path.read_text() == "-- 大事なもの\n"

    @story("B3")
    def it_自分が書いたものは上書きする(repl, tmp_path):
        path = tmp_path / "mine.lean"
        repl.feed("def x := 1")
        repl.feed(f":save {path}")
        repl.feed("def y := 2")
        assert str(path) in repl.feed(f":save {path}")
        assert "def y := 2" in path.read_text()

    @story("B3")
    def it_書けない先でも落ちない(repl, tmp_path):
        repl.feed("def x := 1")
        out = repl.feed(f":save {tmp_path}/no/such/dir/x.lean")
        assert "書き出せなかった" in out, out
        assert "2" in repl.feed("1 + 1")


def describe_init_の扱い():
    """init は base に畳み込むので、環境を作り直すと消えやすい。"""

    @story("F1", "G2")
    def it_作り直しても_init_の宣言が残る(repl, tmp_path, mocker):
        init = tmp_path / "init.lean"
        init.write_text("def fromInit := 99\n")
        mocker.patch.object(leani, "INIT", str(init))
        with contextlib.redirect_stdout(io.StringIO()):
            repl.repl.apply_init()

        assert "99" in repl.feed("fromInit")
        repl.feed(":restart")
        assert "99" in repl.feed("fromInit"), ":restart で init が消えた"

    @story("F2", "G2")
    def it_reset_しても_init_の宣言が残る(repl, tmp_path, mocker):
        init = tmp_path / "init.lean"
        init.write_text("def fromInit := 99\n")
        mocker.patch.object(leani, "INIT", str(init))
        with contextlib.redirect_stdout(io.StringIO()):
            repl.repl.apply_init()

        repl.feed(":reset")
        assert "99" in repl.feed("fromInit"), ":reset で init が消えた"


def describe_書き出しに全部入る():
    """log だけ書き出すと、読み直したファイルが元の環境と違うものになる。"""

    @story("B3", "G2")
    def it_init_と_読み込んだファイルの宣言も書き出す(repl, tmp_path, mocker):
        init = tmp_path / "init.lean"
        init.write_text("def fromInit := 1\n")
        loaded = tmp_path / "loaded.lean"
        loaded.write_text("def fromFile := 2\n")
        out = tmp_path / "all.lean"

        mocker.patch.object(leani, "INIT", str(init))
        with contextlib.redirect_stdout(io.StringIO()):
            repl.repl.apply_init()
        repl.feed(f":l {loaded}")
        repl.feed("def typed := 3")
        repl.feed(f":save {out}")

        text = out.read_text()
        for name in ("fromInit", "fromFile", "typed"):
            assert name in text, f"{name} が書き出されていない:\n{text}"

        repl.feed(":reset")
        repl.feed(f":l {out}")
        assert "1" in repl.feed("fromInit")
        assert "2" in repl.feed("fromFile")
        assert "3" in repl.feed("typed")


def describe_書き直しをやめたとき():
    """遡って続きを書く体勢に入ると、直前の宣言は取り消されている。"""

    @story("C2")
    def it_遡ったまま捨てたら元の宣言が戻る(repl):
        repl.feed("def three := 1")
        repl.feed("  + 2")  # 遡って続きを書く (宣言は取り消されている)
        repl.repl.discard()  # Ctrl-C
        repl.check("discard のあと")

        assert "1" in repl.feed("three"), "取り消した宣言が戻っていない"
        assert len(repl.declarations) == 1

    @story("C2")
    def it_書き直しが確定したら戻さない(repl):
        repl.feed("def three := 1")
        repl.block("  + 2")
        repl.repl.discard()
        assert "3" in repl.feed("three"), "確定した書き直しが巻き戻された"
        assert len(repl.declarations) == 1


def describe_項の位置の_sorry():
    """`:= sorry` はタクティクを差せない。埋め戻しに失敗する。"""

    @story("E3")
    def it_埋め戻せなければ_sorry_版を残す(repl):
        repl.feed("def termSorry : Nat := sorry")
        repl.feed(":prove")
        out = repl.feed("exact 0")
        assert "sorry のまま" in out, out
        # sorry 版が残っているので、名前としては引き続き引ける
        # (#eval は sorry に依存する項を拒むので、値では見ない)。
        assert repl.declarations == ["def termSorry : Nat := sorry"]
        assert "termSorry" in repl.feed(":p termSorry")


def describe_環境が総取り替えになるとき():

    @story("B4", "D3", "E1")
    def it_読み込んだら証明モードから出る(repl, tmp_path):
        # 前の環境の proofState を持ったままだと、送っても噛み合わない。
        path = tmp_path / "other.lean"
        path.write_text("def other := 1\n")
        repl.feed("theorem t5 : 1 = 1 ∧ 2 = 2 := by sorry")
        repl.feed(":prove")
        repl.feed(f":l {path}")
        assert "証明モードではない" in repl.feed(":goals"), "proofState が残った"


def describe_引数の受け取り():

    @story("F2")
    def it_数でない_undo_の引数でも落ちない(repl):
        # 全角の 2。int() に渡すと投げる。
        repl.feed("def a := 1")
        repl.feed("def b := 2")
        out = repl.feed(":undo ²")
        assert len(repl.declarations) == 1, out


def describe_送る前の確認():

    @story("F1")
    def it_起動できていないエンジンには送らない(repl):
        # boot が通らなかったエンジン。import 無しの環境に宣言を積むと、
        # 以後何を書いても通らなくなる (不変条件を確認しないのはそのため)。
        repl.repl.eng.env = None
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            repl.repl.feed("def x := 1")
        assert "使えない" in out.getvalue(), out.getvalue()
        assert repl.declarations == []


def describe_読み込みの中断():

    @story("A4", "F1")
    def it_中断しても環境と宣言を失わない(repl, tmp_path, mocker):
        repl.feed("def before := 5")
        eng = repl.engine
        keep = (eng.env, list(eng.stack), list(eng.log))

        mocker.patch.object(eng, "send_cmd", side_effect=leani.Interrupted())
        with pytest.raises(leani.Interrupted):
            eng.load_file(str(tmp_path / "any.lean"), "def loaded := 1\n")
        assert (eng.env, eng.stack, eng.log) == keep

        mocker.stopall()
        assert "5" in repl.feed("before")


def describe_起動の確かめ():
    """repl は解決できない import を黙って捨てて env を返す。"""

    @story("G4")
    def it_解決できない_import_では起動を断る():
        # そのまま起動すると import Lean も無い環境になり、完結判定も補完も
        # 宣言も全部通らなくなる。起動したように見えるぶんだけ厄介。
        cfg = leani.EnvConfig.make("bogus", imports=["NoSuchModuleXYZ"])
        eng = leani.Engine(cfg)
        try:
            with pytest.raises(leani.EngineDied, match="import が通らない"):
                eng.boot()
        finally:
            eng.kill()

    @story("G3", "G4")
    def it_起動に失敗したエンジンのプロセスを残さない(repl, mocker):
        # Engine を作った時点で repl は起動している。boot が投げたあとに
        # self.eng を差し替えると、殺す手立てが無いまま残る。
        real = leani.Engine.boot
        procs = []

        def only_core(self):
            procs.append(self.proc)
            if self.cfg.name != "core":
                raise leani.EngineDied()
            return real(self)

        mocker.patch.object(leani.Engine, "boot", only_core)
        repl.feed(":env wide")

        dead, live = procs[0], procs[-1]
        assert dead is not live
        assert dead.wait(timeout=10) is not None, "起動に失敗したエンジンが残った"
        assert live.poll() is None, "戻った先のエンジンまで殺した"


def describe_履歴の書き出し():
    """
    write_history_file は毎行ファイルを丸ごと書き直す。読めていないまま
    書くと、前回までの履歴がその 1 行で消える。
    """

    @story("F3")
    def it_読めなかった履歴には書かない(repl, tmp_path, mocker, capsys):
        # libedit は _HiStOrY_V2_ の無い履歴を読めない。GNU readline の
        # 履歴を引き継いだ環境がこれになる。中身はあるので、書けば消える。
        hist = tmp_path / "gnu-history"
        hist.write_text("1 + 1\n2 + 2\n")
        mocker.patch.object(leani, "HIST", str(hist))
        mocker.patch.object(leani.Repl, "_rl_ready", False)
        mocker.patch.object(leani.Repl, "_hist_ok", True)

        repl.repl._setup_readline()
        assert leani.Repl._hist_ok is False, "読めていないのに書きに行く"
        assert "履歴が読めない" in capsys.readouterr().err

        repl.repl._save_history()
        assert hist.read_text() == "1 + 1\n2 + 2\n", "読めない履歴を書き潰した"


def describe_折り返した提案():
    """
    simp? の結果は 100 桁前後で折り返る。取りこぼすと、閉じていない台本を
    「証明完了」として出したまま何も知らせない (sorry が 2 個以上あると
    埋め戻しが走らないので、露見する場所が無い)。
    """

    HYPS = (
        "(hypothesisNumberOne : a = 1) (hypothesisNumberTwo : b = 2) "
        "(hypothesisNumberThree : c = 3) (hypothesisNumberFour : d = 4) "
        "(hypothesisNumberFive : e = 5) (hypothesisNumberSix : f = 6)"
    )
    SIMP = (
        "simp? [hypothesisNumberOne, hypothesisNumberTwo, hypothesisNumberThree, "
        "hypothesisNumberFour, hypothesisNumberFive, hypothesisNumberSix]"
    )

    @story("E2", "E3")
    def it_折り返した提案でも埋め戻せる(repl):
        # 切り落とした台本では埋め戻しが通らず、通ったはずの証明が捨てられる。
        repl.feed(
            f"theorem folded (a b c d e f : Nat) {HYPS} : "
            "a + b + c + d + e + f = 21 := by sorry"
        )
        repl.feed(":prove")
        repl.feed(SIMP)

        decl = repl.declarations[-1]
        assert "sorry" not in decl, decl
        assert "hypothesisNumberSix" in decl, decl
        assert leani.balanced(decl), decl

    @story("E2")
    def it_埋め戻しが走らないときも台本を切らない(repl):
        # sorry が 2 個以上あると close_sorry が即 return するので、壊れた
        # 台本を「証明完了」として出したまま誰も気付けない。
        repl.feed(
            f"theorem twoHoles (a b c d e f : Nat) {HYPS} : "
            "(a + b + c + d + e + f = 21) ∧ True := ⟨sorry, sorry⟩"
        )
        repl.feed(":prove 1")
        out = repl.feed(SIMP)

        assert "証明完了" in out, out
        assert "hypothesisNumberSix" in out, f"台本を切り落とした: {out}"


def describe_sorry_が複数あるとき():
    """
    repl は sorry ごとに位置を返す。テキストの "sorry" を数えて当てていた
    ころは 2 個以上あると埋め戻しを丸ごと諦めていて、それでも「証明完了」
    だけ出ていた (宣言は sorry 版のまま、誰も気付けない)。
    """

    @story("E1", "E3")
    def it_選んだ_sorry_だけを埋め戻す(repl):
        repl.feed("theorem two : 1 = 1 ∧ 2 = 2 := ⟨by sorry, by sorry⟩")
        repl.feed(":prove 1")
        out = repl.feed("rfl")

        assert "証明完了" in out, out
        decl = repl.declarations[-1]
        assert "by rfl" in decl, decl
        assert decl.count("sorry") == 1, f"もう 1 つまで埋めた: {decl}"

    @story("E1", "E3")
    def it_残った_sorry_を続けて証明できる(repl):
        repl.feed("theorem both : 1 = 1 ∧ 2 = 2 := ⟨by sorry, by sorry⟩")
        repl.feed(":prove 1")
        repl.feed("rfl")
        repl.feed(":prove")
        repl.feed("rfl")

        decl = repl.declarations[-1]
        assert "sorry" not in decl, decl

    @story("E1", "E3")
    def it_行頭に寄っている_sorry_のインデントを崩さない(repl):
        # 前後の空白まで動かすと "    sorry" が " trivial" になり、桁が
        # 浅くなって by ブロックから外れる (unexpected identifier)。
        repl.feed("example : True := by\n  have h : True := by\n    sorry\n  exact h")
        repl.feed(":prove")
        out = repl.feed("trivial")

        assert "証明完了" in out, out
        assert "^" not in out, f"埋め戻したものが通っていない: {out}"
        assert repl.declarations[-1] == (
            "example : True := by\n  have h : True := by\n    trivial\n  exact h"
        ), repl.declarations[-1]

    @story("E1", "E3")
    def it_インデントのある宣言でも順に埋められる(repl):
        repl.feed(
            "theorem tidy : True ∧ True := by\n  constructor\n  · sorry\n  · sorry"
        )
        for _ in range(2):
            repl.feed(":prove")
            repl.feed("trivial")

        assert "does not depend on any axioms" in repl.feed("#print axioms tidy")
        assert repl.declarations[0] == (
            "theorem tidy : True ∧ True := by\n  constructor\n  · trivial\n  · trivial"
        ), repl.declarations[0]

    @story("E3")
    def it_埋め戻せなくても残りを続けられる(repl):
        # 項の位置の sorry にはタクティクを差せない。戻すときに持ち越しまで
        # 捨てると、残っている sorry を :prove で続けられなくなる。
        repl.feed("def two : Nat × Nat := (sorry, sorry)")
        repl.feed(":prove 1")
        out = repl.feed("exact 0")

        assert "sorry のまま" in out, out
        assert repl.declarations == ["def two : Nat × Nat := (sorry, sorry)"]
        assert "Nat" in repl.feed(":prove 2"), "残りに入れない"


def describe_タクティクの途中でエンジンが変わる():
    """
    proofState は前のプロセスのもの。新しいエンジンは番号を 0 から振り直す
    ので、そのまま送ると別の証明の状態に当たって返事が来る。
    """

    @story("F1", "E1")
    def it_落ちたら証明モードを畳んで宣言を残す(repl, mocker):
        repl.feed("def keep := 7")
        repl.feed("theorem twoGoals : 1 = 1 ∧ 2 = 2 := by sorry")
        repl.feed(":prove")

        real = leani.Engine.send_tactic
        dead = []

        def die_once(self, src, state):
            if not dead:
                dead.append(src)
                raise leani.EngineDied()
            return real(self, src, state)

        mocker.patch.object(leani.Engine, "send_tactic", die_once)
        out = repl.feed("constructor")

        assert "証明完了" not in out, f"作り直したエンジンの返事を信じた: {out}"
        assert "証明モードを抜けた" in out, out
        assert repl.repl.proof is None

        # 畳んだのに last.proof が立っていると、続きのインデント行で
        # 本物の宣言が pop される (取り消しの控えも消えるので戻せない)。
        mocker.stopall()
        repl.feed("  exact rfl")
        assert "theorem twoGoals : 1 = 1 ∧ 2 = 2 := by sorry" in repl.declarations

    @story("A4", "F1", "E1")
    def it_中断しても証明モードに入り直せる(repl, mocker):
        repl.feed("theorem again : 1 = 1 := by sorry")
        repl.feed(":prove")

        real = leani.Engine.send_tactic
        hit = []

        def stop_once(self, src, state):
            if not hit:
                hit.append(src)
                raise leani.Interrupted()
            return real(self, src, state)

        mocker.patch.object(leani.Engine, "send_tactic", stop_once)
        out = repl.feed("rfl")
        assert "入り直せる" in out, out

        # replay で戻った宣言の sorry を拾い直していないと、宣言はあるのに
        # :prove が「sorry が無い」と言うだけになる。
        mocker.stopall()
        repl.feed(":prove")
        repl.feed("rfl")
        assert "sorry" not in repl.declarations[-1], repl.declarations


def describe_replay_で落としたもの():
    """黙って消えると気付く場所が無い。"""

    @story("F1", "B4")
    def it_読み直せないファイルと消えた宣言を言う(repl, tmp_path):
        path = tmp_path / "lib.lean"
        path.write_text("def libA := 10\n")
        repl.feed(f":l {path}")
        repl.feed("def usesA := libA + 1")

        path.write_text("def libA := (10 : Nat) +\n")  # 直せない形に壊す
        out = repl.feed(":restart")

        assert "読み直せなかった" in out, out
        assert "def usesA" in out, out
        # env に無いものを sources() が並べ続けると、:save したファイルが
        # :l で「すでに宣言されている」と言って通らなくなる。
        assert repl.repl.eng.sources() == [], repl.repl.eng.sources()


def describe_通らなかった宣言の_sorry():
    """
    環境に入っていない宣言の sorry は埋めようがない。持ち越すと、埋め戻しに
    失敗した直後に「sorry 1 個」と出してから「sorry 2 個」と言い直す。
    """

    @story("E1", "E3")
    def it_目標も証明モードの案内も出さない(repl):
        out = repl.feed("theorem ng : True := ⟨by sorry, nonsense⟩")

        assert "proofState" not in out, out
        assert ":prove" not in out, out
        assert repl.repl.pending == []
        assert "sorry が無い" in repl.feed(":prove")

    @story("E1", "E3")
    def it_埋め戻しに失敗しても件数を言い直さない(repl):
        repl.feed("def nums : Nat × Nat := (sorry, sorry)")
        repl.feed(":prove 1")
        out = repl.feed("exact 0")  # 項の位置なのでタクティクは差せない

        assert "sorry のまま" in out, out
        assert out.count("sorry 1 [proofState") == 0, out
        assert "sorry 2 個" in out, out


def describe_replay_が途中で止まったとき():
    """
    流せなかった宣言は控えておくが、環境に入っている宣言の並び (log) には
    混ぜない。log は env のスタックと 1 対 1 で、混ぜると :undo と埋め戻しが
    別の宣言を落とす。
    """

    @story("F1", "F2")
    def it_流せなかった宣言を環境の並びに混ぜない(repl, mocker):
        for one in ("def r1 := 1", "def r2 := 2", "def r3 := 3"):
            repl.feed(one)

        real = leani.Engine.send_cmd
        hit = []

        def once(self, src, **kw):
            if src.startswith("def r2") and not hit:
                hit.append(src)
                raise leani.Interrupted()
            return real(self, src, **kw)

        mocker.patch.object(leani.Engine, "send_cmd", once)
        out = repl.feed(":restart")  # Driver が毎行 INVARIANTS を見る

        assert "まだ流していない宣言: 2 件" in out, out
        assert repl.declarations == ["def r1 := 1"], repl.declarations
        # env に無いものを :save が書くと、書き出したファイルが通らない。
        assert repl.repl.eng.sources() == ["def r1 := 1"], repl.repl.eng.sources()
        assert repl.repl.eng.unplayed == ["def r2 := 2", "def r3 := 3"]

    @story("F1", "F2")
    def it_次の_restart_で流し直す(repl, mocker):
        for one in ("def s1 := 1", "def s2 := 2"):
            repl.feed(one)

        real = leani.Engine.send_cmd
        hit = []

        def once(self, src, **kw):
            if src.startswith("def s2") and not hit:
                hit.append(src)
                raise leani.EngineDied()
            return real(self, src, **kw)

        mocker.patch.object(leani.Engine, "send_cmd", once)
        repl.feed(":restart")
        mocker.stopall()  # エンジンを直してからやり直す

        out = repl.feed(":restart")
        assert "宣言 2 件" in out, out
        assert "s2 : Nat" in repl.feed("#check @s2")
        assert repl.repl.eng.unplayed == []


def describe_書き出したファイルのヘッダ():

    @story("B3")
    def it_lean_でそのまま通る_import_を書く(repl, tmp_path):
        path = tmp_path / "out.lean"
        repl.feed("def savedOne := Lean.versionString")
        repl.feed(f":save {path}")

        # :l は読むときに import Lean を足すので、そこでは露見しない。
        # lean に直接食わせたときだけ Lean.versionString が引けなくなる。
        assert path.read_text().startswith("import Lean\n"), path.read_text()


def describe_init_と読み込みが混ざるとき():

    @story("G2", "B3")
    def it_読み込んだあとも_init_を重ね直す(repl, mocker, tmp_path):
        init = tmp_path / "init.lean"
        init.write_text("def fromInit := 1\n")
        mocker.patch.object(leani, "INIT", str(init))
        repl.repl.apply_init()

        lib = tmp_path / "lib.lean"
        lib.write_text("def fromFile := 2\n")
        repl.feed(f":l {lib}")
        repl.feed("def fromRepl := 3")

        # :l は環境を作り直す。init を重ね直さないと、env には無いものを
        # sources() が並べ続ける。並べる順も流した順でないと通らない。
        assert repl.repl.eng.sources() == [
            "def fromFile := 2",
            "def fromInit := 1",
            "def fromRepl := 3",
        ], repl.repl.eng.sources()
        assert "6" in repl.feed("#eval fromInit + fromFile + fromRepl")


def describe_切り替えに失敗したとき():

    @story("G4", "G5")
    def it_打った宣言も戻ってくる(repl, mocker):
        repl.feed("def typedHere := 42")
        real = leani.Engine.boot

        def only_core(self):
            if self.cfg.name != "core":
                raise leani.EngineDied()
            return real(self)

        mocker.patch.object(leani.Engine, "boot", only_core)
        out = repl.feed(":env wide")

        assert "起動できなかった" in out, out
        # 戻り道は新しい Engine を建てる。:l した中身は preload で戻るが、
        # 対話で打ったぶんは流し直さないと消える。
        assert "42" in repl.feed("#eval typedHere"), "打った宣言が消えた"


def describe_実行時間():
    """重い計算を切り分けるための目安。既定では出さない (毎行のノイズになる)。"""

    @story("A3")
    def it_time_で実行時間が付く(repl):
        assert "on" in repl.feed(":time")
        out = repl.feed("1 + 1")
        assert "2" in out and "s)" in out, out

        assert "off" in repl.feed(":time")
        assert "s)" not in repl.feed("1 + 1")


def describe_少しずつ組み立てる():
    """前に通した宣言を次の宣言から呼べる。REPL の上でだけ形になっていく。"""

    @story("B2")
    def it_通した宣言を次の宣言から呼べる(repl):
        repl.feed("def step1 (n : Nat) := n * 2")
        repl.feed("def step2 (n : Nat) := step1 n + 1")
        assert "11" in repl.feed("#eval step2 5")

    @story("B2", "B3")
    def it_組み立てた順に書き出す(repl, tmp_path):
        path = tmp_path / "grown.lean"
        repl.feed("def step1 (n : Nat) := n * 2")
        repl.feed("def step2 (n : Nat) := step1 n + 1")
        repl.feed(f":save {path}")

        text = path.read_text()
        # 順が入れ替わると lean で通らない (Lean は前方参照を許さない)。
        assert text.index("def step1") < text.index("def step2"), text


def describe_型と_docstring():

    @story("D2")
    def it_i_は型と_docstring_を出す(repl):
        out = repl.feed(":i Nat.succ")
        assert "Nat.succ : Nat → Nat" in out, out
        assert "successor" in out, out

    @story("D2")
    def it_無い名前は断る(repl):
        assert "Nat.nosuchThing" in repl.feed(":i Nat.nosuchThing")


def describe_依存している公理():

    @story("E4")
    def it_print_axioms_が通る(repl):
        repl.feed("theorem ax1 : 1 = 1 := rfl")
        assert "does not depend on any axioms" in repl.feed("#print axioms ax1")

    @story("E4")
    def it_sorry_のままなら_sorryAx_に依存する(repl):
        repl.feed("def ax2 : Nat := sorry")
        assert "sorryAx" in repl.feed("#print axioms ax2")

    @story("E4", "E1", "E3")
    def it_埋め戻したら_sorryAx_が消える(repl):
        # 埋め戻せたかを公理の側から確かめられる。間に環境を進める入力を
        # 挟むと proofState の持ち越しは捨てるので、続けて打つ。
        repl.feed("theorem ax3 : 1 = 1 := by sorry")
        repl.feed(":prove")
        repl.feed("rfl")
        assert "does not depend on any axioms" in repl.feed("#print axioms ax3")


def describe_プローブの途中でエンジンが落ちる():
    """
    完結判定もエンジンへの往復なので、そこで落ちうる。guard が作り直すと
    証明モードは畳まれるが、その行の処理はまだ続いている。
    """

    @story("F1", "E1")
    def it_タクティクの行を宣言として送らない(repl, mocker):
        repl.feed("theorem probeDied : 1 = 1 := by sorry")
        repl.feed(":prove")

        real = leani.Engine.query
        dead = []

        def die_once(self, src, **kw):
            if not dead:
                dead.append(src)
                raise leani.EngineDied()
            return real(self, src, **kw)

        mocker.patch.object(leani.Engine, "query", die_once)
        out = repl.feed("rfl")
        mocker.stopall()

        # 証明モードが畳まれたあとに command 経路へ落ちると、タクティクが
        # 宣言として送られて "expected command" になる。
        assert "expected command" not in out, out
        assert "送らなかった" in out, out
        # そのうえ submit_cmd の clear_pending が拾い直した sorry を消すので、
        # 直前に出した「:prove で入り直せる」が嘘になる。
        assert "入り直せる" in out, out
        assert repl.feed(":prove").count("⊢") >= 1, "案内どおりに入り直せない"


def describe_控えた宣言とエンジンの世代():
    """
    取り消した宣言の控えは env id を持つ。作り直したエンジンにその id は無い。
    """

    @story("F1", "C2")
    def it_死んだ環境_id_を据え直さない(repl):
        repl.feed("def held := 1")
        repl.repl.undone = repl.repl.eng.pop_decl()  # 遡って書き直す途中の形
        repl.feed(":restart")

        repl.repl.restore_undone()  # 書き直さずにやめた
        # 死んだ env id を据えると repl は "Unknown environment." しか返さず、
        # 何を打っても無反応な端末になる。テキストから流し直す。
        assert "2" in repl.feed("#eval held + 1")

    @story("F1")
    def it_環境が食い違ったら黙らない(repl):
        repl.repl.eng.env = 987654  # 死んだ世代の env id を掴んだ状態
        out = repl.feed("def afterGhost := 1")

        assert "environment" in out.lower(), out
        assert ":restart" in out, out


def describe_環境に無い宣言の行き先():
    """
    replay で通らなかった / 流せなかった宣言は env に無い。テキストは控えて
    おくが、そのまま書き出すと通らないファイルになる。
    """

    @story("F1", "F2")
    def it_戻せなかった宣言のテキストを控える(repl, tmp_path):
        path = tmp_path / "lib2.lean"
        path.write_text("def libB := 10\n")
        repl.feed(f":l {path}")
        repl.feed("def usesB := libB + 1")

        path.write_text("def other := 1\n")  # libB を消す
        out = repl.feed(":restart")

        assert "戻せなかった宣言" in out, out
        assert repl.repl.eng.unplayed == ["def usesB := libB + 1"]

    @story("B3", "F2")
    def it_save_はコメントとして添える(repl, tmp_path):
        path = tmp_path / "lib3.lean"
        path.write_text("def libC := 10\n")
        repl.feed(f":l {path}")
        repl.feed("def usesC := libC + 1")
        path.write_text("def other := 1\n")
        repl.feed(":restart")

        out_path = tmp_path / "orphan.lean"
        repl.feed(f":save {out_path}")
        text = out_path.read_text()

        # 落とすと :save は成功を報告したのに打ったものが消える。そのまま
        # 書くと lean で通らない。
        assert "-- def usesC := libC + 1" in text, text
        assert "def other := 1" in text, text

    @story("F2")
    def it_reset_したら控えも捨てる(repl, tmp_path):
        path = tmp_path / "lib4.lean"
        path.write_text("def libD := 10\n")
        repl.feed(f":l {path}")
        repl.feed("def usesD := libD + 1")
        path.write_text("def other := 1\n")
        repl.feed(":restart")

        out = repl.feed(":reset")
        assert "捨てた" in out, out
        assert repl.repl.eng.unplayed == []

        # 残すと、:reset で消したはずの宣言が次の :restart で戻ってくる。
        repl.feed(":restart")
        assert "usesD : Nat" not in repl.feed("#check @usesD")


def describe_読み込んだファイルの_import():

    @story("B3", "B4")
    def it_save_に書き戻す(repl, tmp_path):
        src = tmp_path / "withimport.lean"
        src.write_text("import Lean.Elab.Frontend\n\ndef needsFrontend := 1\n")
        repl.feed(f":l {src}")

        out_path = tmp_path / "kept.lean"
        repl.feed(f":save {out_path}")
        head = out_path.read_text().split("\n\n")[0]

        # 落とすと、:save は成功を報告するのにそのファイルは :l でも lean でも
        # 通らない (Unknown identifier が並ぶ)。
        assert "import Lean.Elab.Frontend" in head, head
        assert "import Lean\n" in head, head


def describe_切り替えが通ったとき():

    @story("G5", "B2")
    def it_打った宣言も新しい環境に入る(repl):
        repl.feed("def carried := 42")
        out = repl.feed(":env wide")

        # 戻り道でだけ流し直していたので、切り替えが成功したときに限って
        # 打った宣言が黙って消えていた。
        assert "42" in repl.feed("#eval carried"), out


def describe_控えと環境の乗り換え():
    """
    replay で通らなかった宣言は控え (`Engine.unplayed`) に残る。環境を作り
    直す操作で黙って消すと、直前に「テキストは控えてある」と言った直後に
    消えることになる。
    """

    def _strand(repl, tmp_path, name):
        """控えを 1 件作る。読み込んだファイルから依存先を消す。"""
        lib = tmp_path / f"{name}.lean"
        lib.write_text(f"def {name}Dep := 1\n")
        repl.feed(f":l {lib}")
        repl.feed(f"def {name}Use := {name}Dep + 1")
        lib.write_text("def other := 9\n")
        repl.feed(":restart")
        assert repl.repl.eng.unplayed == [f"def {name}Use := {name}Dep + 1"]
        return lib

    @story("G5", "F2")
    def it_env_の切り替えでも控えを連れて行く(repl, tmp_path):
        _strand(repl, tmp_path, "envKeep")

        repl.feed(":env wide")
        assert repl.repl.eng.unplayed == ["def envKeepUse := envKeepDep + 1"]

    @story("B4", "F2")
    def it_l_が控えを捨てるならそう言う(repl, tmp_path):
        _strand(repl, tmp_path, "loadDrop")

        other = tmp_path / "other2.lean"
        other.write_text("def zz := 1\n")
        out = repl.feed(f":l {other}")

        assert "捨てた" in out, out
        assert repl.repl.eng.unplayed == []


def describe_コメントに書いた_import():

    @story("B3", "B4")
    def it_save_のヘッダに書かない(repl, tmp_path):
        lib = tmp_path / "doc.lean"
        lib.write_text("/-\nimport Nope.NotAModule\n-/\ndef documented := 1\n")
        repl.feed(f":l {lib}")

        out_path = tmp_path / "doc-out.lean"
        repl.feed(f":save {out_path}")
        head = out_path.read_text().split("\n\n")[0]

        # 書けば repl はヘッダを丸ごと捨てて起動するので、:l でも lean でも
        # 通らないファイルになる。それでも :save は成功を報告する。
        assert "NotAModule" not in head, head
        assert "読み込めなかった" not in repl.feed(f":l {out_path}")


def describe_埋め戻しの途中でエンジンが落ちる():

    @story("E3", "F1")
    def it_通ったのに_sorry_のままと言わない(repl, mocker):
        repl.feed("theorem died : True := by sorry")
        repl.feed(":prove")

        real = leani.Engine.send_cmd
        hit = []

        def die_once(self, src, **kw):
            if "by trivial" in src and not hit:
                hit.append(src)
                raise leani.EngineDied()
            return real(self, src, **kw)

        mocker.patch.object(leani.Engine, "send_cmd", die_once)
        out = repl.feed("trivial")
        mocker.stopall()

        # guard が建て直して再送し、埋め戻した宣言は通っている。世代だけを
        # 見て「sorry のまま」と言うと、sorry 版を流し直して重複エラーの
        # 宣言が控えに永久に居座る (:restart ごとに「戻せなかった宣言」)。
        assert repl.declarations == ["theorem died : True := by trivial"]
        assert "sorry のままにしておく" not in out, out
        assert repl.repl.eng.unplayed == [], repl.repl.eng.unplayed


def describe_作り直しに失敗したときの証明モード():

    @story("F1", "E1")
    def it_死んだ_proofState_に座り続けない(repl, mocker):
        repl.feed("theorem ghost : True := by sorry")
        repl.feed(":prove")

        real = leani.Engine.query
        dead = []

        def die_once(self, src, **kw):
            if not dead:
                dead.append(src)
                raise leani.EngineDied()
            return real(self, src, **kw)

        mocker.patch.object(leani.Engine, "query", die_once)
        mocker.patch.object(leani.Engine, "boot", side_effect=leani.EngineDied())
        out = repl.feed("trivial")

        # 畳まないと、以降どの行も赤い "Unknown proof state." だけを返す
        # 幽霊の証明モードに座り続ける (抜ける案内も出ない)。
        assert "作り直せなかった" in out, out
        assert "証明モードを抜けた" in out, out
        assert repl.repl.proof is None

        mocker.stopall()
        assert "1 件" in repl.feed(":restart")


def describe_起点の環境を失ったとき():
    """
    boot が通らないと環境がどこにも無い。この状態で操作を受けても、嘘の報告を
    せず、打ったテキストを失わないことだけは守る (直せば :restart で戻る)。
    """

    def _no_env(repl, mocker):
        """boot が通らないエンジンにする。控えに宣言 1 件を残す。"""
        repl.feed("def held := 1")
        mocker.patch.object(leani.Engine, "boot", side_effect=leani.EngineDied())
        repl.feed(":restart")
        assert repl.repl.eng.env is None
        assert repl.repl.eng.unplayed == ["def held := 1"]

    @story("F1", "F2")
    def it_reset_は断って控えを残す(repl, mocker):
        _no_env(repl, mocker)

        out = repl.feed(":reset")
        # base は死んだプロセスの id。据え直すと submit の「env が無い」ガードが
        # 外れ、import が 1 つも無い環境に宣言が積まれる (何を書いても通らない)。
        assert "起点の環境が無い" in out, out
        assert repl.repl.eng.unplayed == ["def held := 1"]

        mocker.stopall()
        assert "1 件" in repl.feed(":restart")
        assert "1" in repl.feed("#eval held")

    @story("F1")
    def it_流し直さずに控える(repl, mocker):
        _no_env(repl, mocker)

        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            repl.repl.replay_into(["def two := 2"])
        out = buf.getvalue()

        # 流すと repl は env 無しのリクエストから Init だけの環境を勝手に作る。
        # 「戻した」と報告しながら、設定の import が無い環境に積むことになる。
        assert "戻せなかった" in out, out
        assert repl.repl.eng.unplayed == ["def two := 2", "def held := 1"]
        assert repl.repl.eng.log == []
        repl.check("replay_into のあと")

    @story("F1", "D2")
    def it_型も答えない(repl, mocker):
        _no_env(repl, mocker)

        # 答えると Init だけの環境からの答えになる。設定の import が無いので
        # 嘘になるうえ、「エンジンは健全」に見えてしまう。
        assert "取れなかった" in repl.feed(":t 1 + 1")

    @story("F1", "E1")
    def it_sorry_の持ち越しも捨てる(repl, mocker):
        repl.feed("theorem ghostSorry : True := by sorry")
        assert repl.repl.pending

        real = leani.Engine.query
        dead = []

        def die_once(self, src, **kw):
            if not dead:
                dead.append(src)
                raise leani.EngineDied()
            return real(self, src, **kw)

        mocker.patch.object(leani.Engine, "query", die_once)
        mocker.patch.object(leani.Engine, "boot", side_effect=leani.EngineDied())
        repl.feed("def afterGhost := 1")

        # 残すと :goals が環境に無い宣言の目標を出し、:prove がその幽霊の
        # proofState で証明モードに入る。
        assert repl.repl.pending == []
        assert "sorry" in repl.feed(":prove")  # 「sorry が無い」と言うだけ
        assert repl.repl.proof is None


def describe_解決できない_import_のファイル():

    @story("B4", "G4")
    def it_読み込んだと言わない(repl, tmp_path):
        path = tmp_path / "badimport.lean"
        # 本文は空にしておく。宣言があると「ヘッダを捨てられた環境では
        # 通らない」形で露見してしまい、probe を通らずに弾かれる。
        path.write_text("import NoSuchModuleXYZ\n\n-- 中身は無い\n")
        out = repl.feed(f":l {path}")

        # import が 1 つでも解決できないと repl はヘッダを丸ごと捨てて (エラーも
        # 出さずに) 環境を返す。boot と同じ確かめをしないと、「読み込んだ」と
        # 報告してから完結判定も補完も宣言も通らない環境に座る。
        assert "読み込めなかった" in out, out
        assert "ヘッダ" in out, out
        assert "2" in repl.feed("#eval 1 + 1"), "手元の環境まで失った"
