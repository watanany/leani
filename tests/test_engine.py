"""Repl を直接呼ぶ層。端末は使わないが、本物のエンジンと通信する。

`repl` fixture は 1 行渡すごとに不変条件 (conftest.INVARIANTS) を
確認するので、各テストの assert は「その操作で何が起きてほしいか」だけを
書けばいい。
"""

import contextlib
import io
import json

import pytest
from conftest import story

import leani

# エンジンが PANIC したときのメッセージ。2 行目以降はバックトレース。
PANIC = "PANIC at Foo.bar Foo:1:2: oops\nbacktrace:\n  ..."


def describe_式の評価():

    @story("A1")
    def it_単独の式は評価されて値が表示される(repl):
        assert "2" in repl.feed("1 + 1")

    @story("A1", "B1")
    def it_IO_の式はそのまま実行される(repl):
        assert "hi" in repl.feed('IO.println "hi"')

    @story("A2")
    def it_評価できない項は理由と型を表示する(repl):
        # Nat -> Nat に Repr が無いので #eval できない。
        out = repl.feed("Nat.succ")
        assert "Repr" in out, out
        assert "Nat.succ (n : Nat) : Nat" in out, out

    @story("A2")
    def it_noncomputable_な定数は理由と型を表示する(repl):
        repl.feed("noncomputable def nc : Nat := Classical.choice ⟨1⟩")
        out = repl.feed("nc")
        assert "noncomputable なので評価できない" in out, out
        assert "nc : Nat" in out, out

    @story("A1", "B1")
    def it_do_ブロックは_IO_として読み直す(repl):
        # IO.getEnv は BaseIO なので、Lean は do 全体を BaseIO と決めてしまい、
        # IO.println を書けなくなる。
        out = repl.feed(
            'do let x ← IO.getEnv "LEANI_NO_SUCH_VAR"; IO.println s!"none={x.isNone}"'
        )
        assert "none=true" in out, out
        assert "error" not in out.lower(), out


def describe_複数行の宣言():

    @story("C1")
    def it_空行までをまとめて_1_つの宣言として実行する(repl):
        repl.block(
            "def fib : Nat -> Nat",
            "  | 0 => 0",
            "  | 1 => 1",
            "  | n+2 => fib n + fib (n+1)",
        )
        assert "55" in repl.feed("fib 10")
        assert len(repl.declarations) == 1

    @story("C2")
    def it_確定した直後のインデントした行は前の入力の続きとして扱う(repl):
        repl.feed("def three := 1")
        repl.block("  + 2")
        assert "3" in repl.feed("three")
        assert len(repl.declarations) == 1, "前の入力の続きとして扱わずに 2 件になった"

    @story("C1")
    def it_エラーになった宣言は環境を進めない(repl):
        repl.feed('def broken : Nat := "oops"')
        assert repl.declarations == []

    @story("C6")
    def it_波括弧のあいだは空行があっても_1_つの宣言として実行する(repl):
        repl.feed(":{", "def gap : Nat :=", "", "  5")
        assert repl.declarations == [], ":} の前に実行した"
        repl.feed(":}")
        assert "5" in repl.feed("gap")
        assert len(repl.declarations) == 1

    @story("C6")
    def it_波括弧のあいだが空なら何も実行しない(repl):
        assert repl.feed(":{", ":}") == ""
        assert repl.declarations == []


def describe_証明モード():

    @story("E2")
    def it_exact_の提案をスクリプトに追加する(repl):
        # `exact?` のままだと :save したファイルで毎回検索が実行され、
        # 結果も環境によって変わる。
        repl.feed("theorem t1 (n : Nat) : n + 0 = n := by sorry")
        repl.feed(":prove")
        # 1 つのタクティクで証明が終わるので、そのまま証明モードを終了する。
        out = repl.feed("exact?")
        assert "スクリプトには" in out, out
        assert "証明完了" in out, out
        assert "exact?" not in out[out.index("証明完了") :], "置き換えた宣言に残った"

    @story("E1")
    def it_複数のタクティクを使う証明でゴールとスクリプトを確認できる(repl):
        repl.feed("theorem t2 : 1 = 1 ∧ 2 = 2 := by sorry")
        repl.feed(":prove")
        repl.feed("constructor")
        assert "1 = 1" in repl.feed(":goals")
        assert "constructor" in repl.feed(":script")
        repl.feed("rfl")
        assert "証明完了" in repl.feed("rfl")

    @story("E1")
    def it_証明モードの外で証明用のコマンドを入力すると証明モードではないと伝える(repl):
        # 証明が完成すると leani は自動で証明モードを終了するので、その直後に
        # ユーザーが入力することがある。
        assert "証明モードではない" in repl.feed(":done")
        assert "証明モードではない" in repl.feed(":goals")
        assert "証明モードではない" in repl.feed(":script")

    @story("E1", "F2")
    def it_undo_のあとの_prove_が手前の宣言を消さない(repl):
        repl.feed("def keepme := 42")
        repl.feed("theorem t0 : 1 = 1 := by sorry")
        repl.feed(":undo")
        repl.feed(":prove")
        assert "42" in repl.feed("keepme"), "手前の宣言が巻き戻された"
        assert len(repl.declarations) == 1

    @story("E1")
    def it_undo_でタクティクを_1_つ取り消す(repl):
        repl.feed("theorem t3 : 1 = 1 ∧ 2 = 2 := by sorry")
        repl.feed(":prove")
        repl.feed("constructor")
        repl.feed(":undo")
        assert "∧" in repl.feed(":goals"), "constructor の前のゴールに戻っていない"
        assert "constructor" not in repl.feed(":script")
        assert "取り消せるタクティクが無い" in repl.feed(":undo")

    @story("E1")
    @pytest.mark.parametrize("quit", [":q", ":quit"])
    def it_証明モードの_q_は証明モードだけを終了する(repl, quit):
        repl.feed("theorem t6 : 1 = 1 := by sorry")
        repl.feed(":prove")
        assert "証明モードを終了した" in repl.feed(quit)
        assert repl.repl.proof is None
        assert "2" in repl.feed("1 + 1"), "REPL まで終了した"

    @story("E1")
    def it_証明モードの外の_goals_で残っている_sorry_を表示する(repl):
        repl.feed("theorem t4 : 3 = 3 := by sorry")
        out = repl.feed(":goals")
        assert "sorry 1" in out, out
        assert "3 = 3" in out, out


def describe_ファイルの読み書き():

    @story("B3", "B4")
    def it_save_したファイルを読み込み直せる(repl, tmp_path):
        path = tmp_path / "saved.lean"
        repl.feed("def saved := 7")
        assert str(path) in repl.feed(f":save {path}")
        assert "def saved := 7" in path.read_text()
        repl.feed(":reset")
        repl.feed(f":l {path}")
        assert "7" in repl.feed("saved")

    @story("B4", "F1")
    def it_読み込みに失敗しても今の環境を失わない(repl, tmp_path):
        # env を捨てたまま戻さないと、以後の入力が import 無しの環境に送られて
        # 何を書いてもエラーになる。
        broken = tmp_path / "broken.lean"
        broken.write_text('def broken : Nat := "oops"\n')
        repl.feed("def before := 5")
        assert "読み込めなかった" in repl.feed(f":l {broken}")
        assert "5" in repl.feed("before")

    @story("B4")
    def it_読み込めないファイルは理由を表示する(repl, tmp_path, mocker):
        path = tmp_path / "locked.lean"
        path.write_text("def x := 1\n")
        mocker.patch(
            "leani.repl.open",
            create=True,
            side_effect=PermissionError(13, "Permission denied"),
        )
        assert "読めない" in repl.feed(f":l {path}")

    @story("B4")
    def it_編集したファイルを_r_で読み込み直す(repl, tmp_path):
        path = tmp_path / "edited.lean"
        path.write_text("def edited := 1\n")
        repl.feed(f":l {path}")
        path.write_text("def edited := 2\n")
        repl.feed(":r")
        assert "2" in repl.feed("edited"), repl.feed("edited")

    @story("B4", "F2")
    def it_ファイルを読み込んでいなければ_r_は起動直後に戻す(repl):
        repl.feed("def gone := 1")
        repl.feed(":r")
        assert repl.declarations == []


def describe_エンジンが異常終了してもセッションが続く():

    @story("F1")
    def it_エンジンが異常終了したら再起動して宣言を_replay_する(repl, mocker):
        repl.feed("def survivor := 9")
        real = repl.engine.send_cmd
        crashed = []

        def crash_once(src, fresh=False):
            if not crashed and src.startswith("def afterCrash"):
                crashed.append(src)
                raise leani.types.EngineDied()
            return real(src, fresh=fresh)

        mocker.patch.object(repl.engine, "send_cmd", side_effect=crash_once)
        out = repl.feed("def afterCrash := 3")
        assert "再起動する" in out, out
        assert "1 件を再実行" in out, out
        assert "9" in repl.feed("survivor"), "replay されていない"
        assert "3" in repl.feed("afterCrash"), (
            "異常終了のときに送っていた宣言が再実行されていない"
        )
        assert repl.declarations == ["def survivor := 9", "def afterCrash := 3"]

    @story("A4", "F1")
    def it_再起動中にもう一度中断しても応答がずれない(repl, mocker):
        repl.feed("def before := 5")
        eng = repl.engine
        real_cmd, real_exchange = eng.send_cmd, eng._exchange
        state = {"first": True, "second": False}

        def interrupt_once(src, fresh=False):
            if state["first"] and src.startswith("#eval 0"):
                state["first"], state["second"] = False, True
                raise leani.types.Interrupted()
            return real_cmd(src, fresh=fresh)

        def interrupt_replay(obj):
            # replay で宣言を送った直後、レスポンスを読む前に中断する。
            if state["second"] and obj.get("cmd") == "def before := 5":
                state["second"] = False
                eng.proc.stdin.write(json.dumps(obj) + "\n\n")
                eng.proc.stdin.flush()
                raise KeyboardInterrupt
            return real_exchange(obj)

        mocker.patch.object(eng, "send_cmd", side_effect=interrupt_once)
        mocker.patch.object(eng, "_exchange", side_effect=interrupt_replay)
        repl.feed("#eval 0")
        assert not state["second"], "replay の途中で中断していない"
        assert "5" in repl.feed("before")
        assert "2" in repl.feed("1 + 1")

    @story("F1")
    def it_エンジンが_PANIC_したら宣言を環境に追加せず警告する(repl, mocker):
        # PANIC したあとのエンジンは状態が壊れている可能性があるので、結果を
        # 普通の結果として扱わない。
        real = repl.engine.send_cmd
        panic = {"env": 99, "messages": [{"severity": "info", "data": PANIC}]}

        def panic_on_def(src, fresh=False):
            return panic if src.startswith("def panicked") else real(src, fresh=fresh)

        mocker.patch.object(repl.engine, "send_cmd", side_effect=panic_on_def)
        out = repl.feed("def panicked := 1")
        assert "PANIC した" in out, out
        assert "PANIC at Foo.bar" in out, out
        assert repl.declarations == []

    @story("F1")
    def it_式の評価で_PANIC_したら結果を表示せず警告する(repl, mocker):
        real = repl.engine.send_cmd
        panic = {"messages": [{"severity": "info", "data": "42\n" + PANIC}]}

        def panic_on_eval(src, fresh=False):
            wrapped = src.startswith("#eval") and "panicky" in src
            return panic if wrapped else real(src, fresh=fresh)

        mocker.patch.object(repl.engine, "send_cmd", side_effect=panic_on_eval)
        out = repl.feed("panicky + 1")
        assert "PANIC した" in out, out
        assert "42" not in out, out


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
    def it_設定に無い名前を指定してもセッションが続く(repl):
        assert "設定に無い" in repl.feed(":env nosuch")
        assert "2" in repl.feed("1 + 1"), "環境が壊れてセッションが終了している"

    @story("G4", "G5")
    def it_切り替え先が起動できなければ元の環境に戻る(repl, mocker):
        # import が成功するかは boot するまで分からない。今のエンジンは kill 済み
        # なので、そのまま例外を投げるとセッションごと消える。
        real = leani.engine.Engine.boot

        def only_core(self):
            if self.cfg.name != "core":
                raise leani.types.EngineDied()
            return real(self)

        mocker.patch.object(leani.engine.Engine, "boot", only_core)
        out = repl.feed(":env wide")
        assert "起動できなかった" in out, out
        assert repl.repl.cfg.name == "core", "元の環境に戻っていない"
        assert "2" in repl.feed("1 + 1"), "使えなくなっている"

    @story("G5")
    def it_引数の無い_env_は今の環境と切り替え先を表示する(repl):
        out = repl.feed(":env")
        assert "core" in out, out
        assert "切り替え先: core wide" in out, out

    @story("G5")
    def it_今の環境の名前を指定したら再起動しない(repl):
        repl.feed("def stay := 1")
        eng = repl.engine
        assert "すでに core" in repl.feed(":env core")
        assert repl.engine is eng, "再起動した"


def describe_コマンドの一覧():

    @story("H1")
    def it_help_でコマンドの一覧を表示する(repl):
        out = repl.feed(":help")
        assert ":prove" in out, out
        assert ":env" in out, out

    @story("H1")
    def it_証明モードの_help_では証明用のコマンドを表示する(repl):
        repl.feed("theorem t5 : 1 = 1 := by sorry")
        repl.feed(":prove")
        assert "1 行が 1 タクティク" in repl.feed(":help")

    @story("H1", "F4")
    def it_無いコマンドを入力したら知らせてセッションを続ける(repl):
        assert "不明なコマンド: :nosuch" in repl.feed(":nosuch")
        assert "2" in repl.feed("1 + 1")


def describe_エンジンを再起動できないとき():
    """boot が失敗することもある。leani は報告して続ける (異常終了しない)。"""

    @story("F1")
    def it_再起動に失敗しても報告して異常終了しない(repl, mocker):
        repl.feed("def kept := 4")
        mocker.patch.object(
            leani.engine.Engine, "boot", side_effect=leani.types.EngineDied()
        )
        out = repl.feed(":restart")
        assert "再起動できなかった" in out, out

    @story("F1")
    def it_失敗しても宣言を捨てない(repl, mocker):
        # 捨てると、原因を直して再実行しても replay できなくなる。ただし log には
        # 残せない (env が無いので「環境に含まれている宣言」ではない)。
        repl.feed("def kept := 4")
        mocker.patch.object(
            leani.engine.Engine, "boot", side_effect=leani.types.EngineDied()
        )
        repl.feed(":restart")
        assert repl.repl.eng.unplayed == ["def kept := 4"], repl.repl.eng.unplayed
        assert repl.repl.eng.log == []

        mocker.stopall()
        assert "1 件を再実行" in repl.feed(":restart")
        assert "4" in repl.feed("kept")


def describe_書き出しの安全対策():
    """:save は宣言を残す操作。そこで宣言を失わせない。"""

    @story("B3")
    def it_すでにあるファイルは上書きしない(repl, tmp_path):
        path = tmp_path / "existing.lean"
        path.write_text("-- 大事なもの\n")
        repl.feed("def x := 1")
        assert "すでにある" in repl.feed(f":save {path}")
        assert path.read_text() == "-- 大事なもの\n"

    @story("B3")
    def it_leani_が書き出したファイルは上書きする(repl, tmp_path):
        path = tmp_path / "mine.lean"
        repl.feed("def x := 1")
        repl.feed(f":save {path}")
        repl.feed("def y := 2")
        assert str(path) in repl.feed(f":save {path}")
        assert "def y := 2" in path.read_text()

    @story("B3")
    def it_書き込めない場所を指定しても異常終了しない(repl, tmp_path):
        repl.feed("def x := 1")
        out = repl.feed(f":save {tmp_path}/no/such/dir/x.lean")
        assert "書き出せなかった" in out, out
        assert "2" in repl.feed("1 + 1")


def describe_init_の扱い():
    """init は base の環境に含めるので、環境を作り直すと消えやすい。"""

    @story("F1", "G2")
    def it_再起動しても_init_の宣言が残る(repl, tmp_path, mocker):
        init = tmp_path / "init.lean"
        init.write_text("def fromInit := 99\n")
        mocker.patch.object(leani.repl, "INIT", str(init))
        with contextlib.redirect_stdout(io.StringIO()):
            repl.repl.apply_init()

        assert "99" in repl.feed("fromInit")
        repl.feed(":restart")
        assert "99" in repl.feed("fromInit"), ":restart で init が消えた"

    @story("F2", "G2")
    def it_reset_しても_init_の宣言が残る(repl, tmp_path, mocker):
        init = tmp_path / "init.lean"
        init.write_text("def fromInit := 99\n")
        mocker.patch.object(leani.repl, "INIT", str(init))
        with contextlib.redirect_stdout(io.StringIO()):
            repl.repl.apply_init()

        repl.feed(":reset")
        assert "99" in repl.feed("fromInit"), ":reset で init が消えた"


def describe_書き出しにすべての宣言を含める():
    """log だけ書き出すと、読み込み直したファイルが元の環境と違うものになる。"""

    @story("B3", "G2")
    def it_init_と_読み込んだファイルの宣言も書き出す(repl, tmp_path, mocker):
        init = tmp_path / "init.lean"
        init.write_text("def fromInit := 1\n")
        loaded = tmp_path / "loaded.lean"
        loaded.write_text("def fromFile := 2\n")
        out = tmp_path / "all.lean"

        mocker.patch.object(leani.repl, "INIT", str(init))
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
    """前の宣言の続きを書き始めた時点で、直前の宣言は取り消されている。"""

    @story("C2")
    def it_続きを書く途中で捨てたら元の宣言が戻る(repl):
        repl.feed("def three := 1")
        repl.feed("  + 2")  # 前の宣言の続きを書く (宣言は取り消されている)
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
    """`:= sorry` の位置にはタクティクを書けないので、置き換えに失敗する。"""

    @story("E3")
    def it_置き換えられなければ_sorry_のまま残す(repl):
        repl.feed("def termSorry : Nat := sorry")
        repl.feed(":prove")
        out = repl.feed("exact 0")
        assert "sorry のまま" in out, out
        # sorry のままの宣言が残っているので、名前としては引き続き参照できる。
        # 値では確かめない。sorry に依存する項は、Lean が評価を拒むため。
        assert repl.declarations == ["def termSorry : Nat := sorry"]
        assert "termSorry" in repl.feed(":p termSorry")


def describe_環境全体が置き換わるとき():

    @story("B4", "E1")
    def it_読み込んだら証明モードを終了する(repl, tmp_path):
        # 前の環境の proofState を持ったままだと、送っても新しい環境と合わない。
        path = tmp_path / "other.lean"
        path.write_text("def other := 1\n")
        repl.feed("theorem t5 : 1 = 1 ∧ 2 = 2 := by sorry")
        repl.feed(":prove")
        repl.feed(f":l {path}")
        assert "証明モードではない" in repl.feed(":goals"), "proofState が残った"


def describe_引数の受け取り():

    @story("F2")
    def it_undo_の引数が数でなくても異常終了しない(repl):
        # 上付きの ²。isdigit() は True を返すが、int() は変換できない。
        repl.feed("def a := 1")
        repl.feed("def b := 2")
        out = repl.feed(":undo ²")
        assert len(repl.declarations) == 1, out


def describe_送る前の確認():

    @story("F1")
    def it_起動できていないエンジンには送らない(repl):
        # boot に失敗したエンジン。import 無しの環境に宣言を追加すると、
        # 以後何を書いてもエラーになる。
        repl.repl.eng.env = None
        out = repl.feed("def x := 1")
        assert "使えない" in out, out
        assert repl.declarations == []


def describe_読み込みの中断():

    @story("A4", "F1")
    def it_中断しても環境と宣言を失わない(repl, tmp_path, mocker):
        repl.feed("def before := 5")
        eng = repl.engine
        keep = (eng.env, list(eng.stack), list(eng.log))

        mocker.patch.object(eng, "send_cmd", side_effect=leani.types.Interrupted())
        with pytest.raises(leani.types.Interrupted):
            eng.load_file(str(tmp_path / "any.lean"), "def loaded := 1\n")
        assert (eng.env, eng.stack, eng.log) == keep

        mocker.stopall()
        assert "5" in repl.feed("before")


def describe_起動時の確認():
    """repl は解決できない import をエラーにせずに捨てて、env を返す。"""

    @story("G4")
    def it_解決できない_import_では起動を中止する():
        # そのまま起動すると import Lean も無い環境になり、完結判定も補完も
        # 宣言もすべて失敗する。起動したように見えるので、原因に気付きにくい。
        cfg = leani.config.EnvConfig.make("bogus", imports=["NoSuchModuleXYZ"])
        eng = leani.engine.Engine(cfg)
        try:
            with pytest.raises(leani.types.EngineDied, match="import に失敗した"):
                eng.boot()
        finally:
            eng.kill()

    @story("G4")
    def it_エンジンが異常終了したら終了シグナルや終了コードを報告に含める():
        # バージョンの合わないエンジンを読み込むと、repl は何も出力せずに終了する。
        # 終了シグナルや終了コードを報告に含めないと、呼び出し側は理由を伝えられず、
        # 「import に失敗した」という推測だけが表示される。ユーザーは書き間違えて
        # いない import を疑うことになる。
        cfg = leani.config.EnvConfig.make("bogus")
        eng = leani.engine.Engine(cfg)
        try:
            assert eng.proc is not None
            eng.proc.kill()
            with pytest.raises(leani.types.EngineDied, match="SIGKILL"):
                eng.send_cmd("def after_kill := 1", fresh=True)
        finally:
            eng.kill()

    @story("G4", "G5")
    def it_起動に失敗したエンジンのプロセスを残さない(repl, mocker):
        # Engine を作った時点で repl は起動している。boot が投げたあとに
        # self.eng を差し替えると、終了させる方法が無いままプロセスが残る。
        real = leani.engine.Engine.boot
        procs = []

        def only_core(self):
            procs.append(self.proc)
            if self.cfg.name != "core":
                raise leani.types.EngineDied()
            return real(self)

        mocker.patch.object(leani.engine.Engine, "boot", only_core)
        repl.feed(":env wide")

        dead, live = procs[0], procs[-1]
        assert dead is not live
        assert dead.wait(timeout=10) is not None, "起動に失敗したエンジンが残った"
        assert live.poll() is None, "戻った先のエンジンまで終了させた"


def describe_履歴の書き出し():
    """
    履歴は確定した入力を 1 件として、そのつど追記する。ファイル全体を書き直さないので、
    読めない形式のファイルがあっても消さない。
    """

    @story("F3")
    def it_読めない形式の履歴を上書きしない(tmp_path):
        # readline や libedit の履歴を引き継いだ環境ではこの形式になる。読み込むときは
        # 無視されるが、書き込みは追記なので前の履歴は残る。
        hist = tmp_path / "gnu-history"
        hist.write_text("1 + 1\n2 + 2\n")

        leani.repl.BlockHistory(str(hist)).record("3 + 3")
        assert hist.read_text().startswith("1 + 1\n2 + 2\n"), "前の履歴が消えた"

    @story("C3", "F3")
    def it_複数行の入力を_1_件として保存する(tmp_path):
        history = leani.repl.BlockHistory(str(tmp_path / "history"))
        history.record("def f : Nat -> Nat\n  | 0 => 1")
        assert list(history.load_history_strings()) == [
            "def f : Nat -> Nat\n  | 0 => 1"
        ]

    @story("C3", "F3")
    def it_行ごとの追加は受け付けない(tmp_path):
        # prompt_toolkit は prompt() から戻るたびに 1 行追加しようとする。受け付けると
        # 複数行の宣言が行ごとに分かれ、呼び戻すのに Ctrl-P が何度も必要になる。
        history = leani.repl.BlockHistory(str(tmp_path / "history"))
        history.append_string("  | 0 => 1")
        assert list(history.load_history_strings()) == []


def describe_折り返した提案():
    """
    simp? の結果は 100 桁前後で折り返される。2 行目以降を取り出さないと、閉じていない
    スクリプトを「証明完了」として表示したまま何も知らせない (sorry が 2 個以上あると
    置き換えを実行しないので、問題に気付く場所が無い)。
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
    def it_折り返した提案でも_sorry_を置き換えられる(repl):
        # 途中で切れたスクリプトでは置き換えが失敗し、成功したはずの証明が捨てられる。
        repl.feed(
            f"theorem folded (a b c d e f : Nat) {HYPS} : "
            "a + b + c + d + e + f = 21 := by sorry"
        )
        repl.feed(":prove")
        repl.feed(SIMP)

        decl = repl.declarations[-1]
        assert "sorry" not in decl, decl
        assert "hypothesisNumberSix" in decl, decl
        assert leani.pure.balanced(decl), decl

    @story("E2")
    def it_置き換えを実行しないときもスクリプトを切り詰めない(repl):
        # sorry が 2 個以上あると close_sorry がすぐに return するので、壊れた
        # スクリプトを「証明完了」として表示したまま誰も気付けない。
        repl.feed(
            f"theorem twoHoles (a b c d e f : Nat) {HYPS} : "
            "(a + b + c + d + e + f = 21) ∧ True := ⟨sorry, sorry⟩"
        )
        repl.feed(":prove 1")
        out = repl.feed(SIMP)

        assert "証明完了" in out, out
        assert "hypothesisNumberSix" in out, f"スクリプトを途中で切った: {out}"


def describe_sorry_が複数あるとき():
    """
    repl は sorry ごとに位置を返すので、leani はその位置で 1 個だけを置き換える。
    テキストの "sorry" を数えて位置を決めると、2 個以上あるときに置き換えられず、
    宣言が sorry のままなのに「証明完了」と表示することになる。
    """

    @story("E1", "E3")
    def it_選んだ_sorry_だけを置き換える(repl):
        repl.feed("theorem two : 1 = 1 ∧ 2 = 2 := ⟨by sorry, by sorry⟩")
        repl.feed(":prove 1")
        out = repl.feed("rfl")

        assert "証明完了" in out, out
        decl = repl.declarations[-1]
        assert "by rfl" in decl, decl
        assert decl.count("sorry") == 1, f"もう 1 つの sorry まで置き換えた: {decl}"

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
    def it_単独の行にある_sorry_のインデントを崩さない(repl):
        # 前後の空白まで変えると "    sorry" が " trivial" になり、インデントが
        # 浅くなって by ブロックの外に出る (unexpected identifier)。
        repl.feed("example : True := by\n  have h : True := by\n    sorry\n  exact h")
        repl.feed(":prove")
        out = repl.feed("trivial")

        assert "証明完了" in out, out
        assert "^" not in out, f"置き換えた宣言がエラーになっている: {out}"
        assert repl.declarations[-1] == (
            "example : True := by\n  have h : True := by\n    trivial\n  exact h"
        ), repl.declarations[-1]

    @story("E1", "E3")
    def it_インデントのある宣言でも順に置き換えられる(repl):
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
    def it_置き換えられなくても残りの_sorry_の証明を続けられる(repl):
        # 項の位置の sorry にはタクティクを書けない。元に戻すときに持ち越した sorry まで
        # 捨てると、残っている sorry の証明を :prove で続けられなくなる。
        repl.feed("def two : Nat × Nat := (sorry, sorry)")
        repl.feed(":prove 1")
        out = repl.feed("exact 0")

        assert "sorry のまま" in out, out
        assert repl.declarations == ["def two : Nat × Nat := (sorry, sorry)"]
        assert "Nat" in repl.feed(":prove 2"), "残りの sorry の証明を始められない"


def describe_タクティクの途中でエンジンが変わる():
    """
    proofState は前のプロセスのもの。新しいエンジンは番号を 0 から付け直す
    ので、そのまま送ると別の証明の状態に対する応答が返ってくる。
    """

    @story("F1", "E1")
    def it_エンジンが異常終了したら証明モードを終了して宣言を残す(repl, mocker):
        repl.feed("def keep := 7")
        repl.feed("theorem twoGoals : 1 = 1 ∧ 2 = 2 := by sorry")
        repl.feed(":prove")

        real = leani.engine.Engine.send_tactic
        dead = []

        def die_once(self, src, state):
            if not dead:
                dead.append(src)
                raise leani.types.EngineDied()
            return real(self, src, state)

        mocker.patch.object(leani.engine.Engine, "send_tactic", die_once)
        out = repl.feed("constructor")

        assert "証明完了" not in out, f"再起動したエンジンの応答をそのまま使った: {out}"
        assert "証明モードを終了した" in out, out
        assert repl.repl.proof is None

        # 証明モードを終了したのに last.proof が立っていると、続きのインデントした行で
        # 実際の宣言が pop される (取り消した中身も消えるので元に戻せない)。
        mocker.stopall()
        repl.feed("  exact rfl")
        assert "theorem twoGoals : 1 = 1 ∧ 2 = 2 := by sorry" in repl.declarations

    @story("A4", "F1", "E1")
    def it_中断しても証明モードを再開できる(repl, mocker):
        repl.feed("theorem again : 1 = 1 := by sorry")
        repl.feed(":prove")

        real = leani.engine.Engine.send_tactic
        hit = []

        def stop_once(self, src, state):
            if not hit:
                hit.append(src)
                raise leani.types.Interrupted()
            return real(self, src, state)

        mocker.patch.object(leani.engine.Engine, "send_tactic", stop_once)
        out = repl.feed("rfl")
        assert "再開できる" in out, out

        # replay で戻った宣言の sorry を取得し直していないと、宣言はあるのに
        # :prove が「sorry が無い」と表示するだけになる。
        mocker.stopall()
        repl.feed(":prove")
        repl.feed("rfl")
        assert "sorry" not in repl.declarations[-1], repl.declarations


def describe_replay_で再実行できなかったもの():
    """何も表示せずに消えると、ユーザーが気付く場所が無い。"""

    @story("F1", "B4")
    def it_読み込み直せないファイルと消えた宣言を報告する(repl, tmp_path):
        path = tmp_path / "lib.lean"
        path.write_text("def libA := 10\n")
        repl.feed(f":l {path}")
        repl.feed("def usesA := libA + 1")

        path.write_text("def libA := (10 : Nat) +\n")  # 直せない形に壊す
        out = repl.feed(":restart")

        assert "読み込み直せなかった" in out, out
        assert "def usesA" in out, out
        # env に無いものを sources() が並べ続けると、:save したファイルが
        # :l で「すでに宣言されている」というエラーになる。
        assert repl.repl.eng.sources() == [], repl.repl.eng.sources()


def describe_エラーになった宣言の_sorry():
    """
    環境に含まれていない宣言の sorry は置き換えられない。持ち越すと、置き換えに
    失敗した直後に「sorry 1 個」と表示してから「sorry 2 個」と表示し直す。
    """

    @story("E1", "E3")
    def it_ゴールも証明モードの案内も表示しない(repl):
        out = repl.feed("theorem ng : True := ⟨by sorry, nonsense⟩")

        assert "proofState" not in out, out
        assert ":prove" not in out, out
        assert repl.repl.pending == []
        assert "sorry が無い" in repl.feed(":prove")

    @story("E1", "E3")
    def it_置き換えに失敗しても件数を報告し直さない(repl):
        repl.feed("def nums : Nat × Nat := (sorry, sorry)")
        repl.feed(":prove 1")
        out = repl.feed("exact 0")  # 項の位置なのでタクティクは書けない

        assert "sorry のまま" in out, out
        assert out.count("sorry 1 [proofState") == 0, out
        assert "sorry 2 個" in out, out
        assert repl.declarations == ["def nums : Nat × Nat := (sorry, sorry)"]


def describe_replay_が途中で止まったとき():
    """
    実行できなかった宣言は保留しておくが、環境に含まれている宣言の並び (log) には
    混ぜない。log は env のスタックと 1 対 1 に対応していて、混ぜると :undo と
    sorry の置き換えが別の宣言を消してしまう。
    """

    @story("F1", "F2")
    def it_実行できなかった宣言を環境の並びに混ぜない(repl, mocker):
        for one in ("def r1 := 1", "def r2 := 2", "def r3 := 3"):
            repl.feed(one)

        real = leani.engine.Engine.send_cmd
        hit = []

        def once(self, src, **kw):
            if src.startswith("def r2") and not hit:
                hit.append(src)
                raise leani.types.Interrupted()
            return real(self, src, **kw)

        mocker.patch.object(leani.engine.Engine, "send_cmd", once)
        out = repl.feed(":restart")  # Driver が毎行 INVARIANTS を見る

        assert "再実行しなかった宣言: 2 件" in out, out
        assert repl.declarations == ["def r1 := 1"], repl.declarations
        # env に無いものを :save が書くと、書き出したファイルがエラーになる。
        assert repl.repl.eng.sources() == ["def r1 := 1"], repl.repl.eng.sources()
        assert repl.repl.eng.unplayed == ["def r2 := 2", "def r3 := 3"]

    @story("F1", "F2")
    def it_次の_restart_で再実行する(repl, mocker):
        for one in ("def s1 := 1", "def s2 := 2"):
            repl.feed(one)

        real = leani.engine.Engine.send_cmd
        hit = []

        def once(self, src, **kw):
            if src.startswith("def s2") and not hit:
                hit.append(src)
                raise leani.types.EngineDied()
            return real(self, src, **kw)

        mocker.patch.object(leani.engine.Engine, "send_cmd", once)
        repl.feed(":restart")
        mocker.stopall()  # エンジンを直してから再実行する

        out = repl.feed(":restart")
        assert "宣言 2 件" in out, out
        assert "s2 : Nat" in repl.feed("#check @s2")
        assert repl.repl.eng.unplayed == []


def describe_書き出したファイルのヘッダ():

    @story("B3")
    def it_lean_でそのまま実行できる_import_を書く(repl, tmp_path):
        path = tmp_path / "out.lean"
        repl.feed("def savedOne := Lean.versionString")
        repl.feed(f":save {path}")

        # :l は読み込むときに import Lean を足すので、:l では問題が見つからない。
        # lean に直接渡したときだけ Lean.versionString が見つからなくなる。
        assert path.read_text().startswith("import Lean\n"), path.read_text()


def describe_init_と読み込みが混ざるとき():

    @story("G2", "B3")
    def it_読み込んだあとも_init_を追加し直す(repl, mocker, tmp_path):
        init = tmp_path / "init.lean"
        init.write_text("def fromInit := 1\n")
        mocker.patch.object(leani.repl, "INIT", str(init))
        repl.repl.apply_init()

        lib = tmp_path / "lib.lean"
        lib.write_text("def fromFile := 2\n")
        repl.feed(f":l {lib}")
        repl.feed("def fromRepl := 3")

        # :l は環境を作り直す。init を追加し直さないと、env には無いものを
        # sources() が並べ続ける。並べる順も実行した順でないとエラーになる。
        assert repl.repl.eng.sources() == [
            "def fromFile := 2",
            "def fromInit := 1",
            "def fromRepl := 3",
        ], repl.repl.eng.sources()
        assert "6" in repl.feed("#eval fromInit + fromFile + fromRepl")


def describe_切り替えに失敗したとき():

    @story("G4", "G5")
    def it_入力した宣言も元の環境に残る(repl, mocker):
        repl.feed("def typedHere := 42")
        real = leani.engine.Engine.boot

        def only_core(self):
            if self.cfg.name != "core":
                raise leani.types.EngineDied()
            return real(self)

        mocker.patch.object(leani.engine.Engine, "boot", only_core)
        out = repl.feed(":env wide")

        assert "起動できなかった" in out, out
        # 元の環境に戻るときは新しい Engine を作る。:l したファイルの内容は preload で
        # 戻るが、対話で入力した宣言は再実行しないと消える。
        assert "42" in repl.feed("#eval typedHere"), "入力した宣言が消えた"


def describe_実行時間():
    """重い計算を切り分けるための目安。デフォルトでは表示しない (ノイズになる)。"""

    @story("A3")
    def it_time_で実行時間を表示する(repl):
        assert "on" in repl.feed(":time")
        out = repl.feed("1 + 1")
        assert "2" in out, out
        assert "s)" in out, out

        assert "off" in repl.feed(":time")
        assert "s)" not in repl.feed("1 + 1")


def describe_少しずつ組み立てる():
    """前に実行した宣言を次の宣言から呼べる。REPL 上で少しずつ組み立てる。"""

    @story("B2")
    def it_実行した宣言を次の宣言から呼べる(repl):
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
        # 順番が入れ替わると lean でエラーになる (Lean は前方参照を許さない)。
        assert text.index("def step1") < text.index("def step2"), text


def describe_型と_docstring():

    @story("D2")
    def it_i_は型と_docstring_を表示する(repl):
        out = repl.feed(":i Nat.succ")
        assert "Nat.succ : Nat → Nat" in out, out
        assert "successor" in out, out

    @story("D2")
    def it_無い名前はエラーを表示する(repl):
        assert "Nat.nosuchThing" in repl.feed(":i Nat.nosuchThing")


def describe_型と定義の表示():

    @story("D2")
    @pytest.mark.parametrize("cmd", [":t", ":type"])
    def it_t_は式の型を表示する(repl, cmd):
        assert "Nat" in repl.feed(f"{cmd} 1 + 1")

    @story("D3")
    def it_p_は定義の本体を表示する(repl):
        repl.feed("def shown := 1 + 2")
        assert "1 + 2" in repl.feed(":p shown")

    @story("D3", "H1")
    def it_p_に名前が無ければ名前が必要と伝える(repl):
        assert ":p には名前が必要" in repl.feed(":p")


def describe_宣言の取り消し():

    @story("F2")
    def it_undo_n_で_n_件取り消す(repl):
        repl.feed("def u1 := 1")
        repl.feed("def u2 := 2")
        repl.feed("def u3 := 3")
        repl.feed(":undo 2")
        assert repl.declarations == ["def u1 := 1"]
        assert "1" in repl.feed("u1")


def describe_依存している公理():

    @story("E4")
    def it_print_axioms_を実行できる(repl):
        repl.feed("theorem ax1 : 1 = 1 := rfl")
        assert "does not depend on any axioms" in repl.feed("#print axioms ax1")

    @story("E4")
    def it_sorry_のままなら_sorryAx_に依存する(repl):
        repl.feed("def ax2 : Nat := sorry")
        assert "sorryAx" in repl.feed("#print axioms ax2")

    @story("E4", "E1", "E3")
    def it_置き換えたら_sorryAx_が消える(repl):
        # 置き換えられたかを公理の側から確認できる。あいだに環境を進める入力を
        # 挟むと持ち越した proofState を捨てるので、続けて入力する。
        repl.feed("theorem ax3 : 1 = 1 := by sorry")
        repl.feed(":prove")
        repl.feed("rfl")
        assert "does not depend on any axioms" in repl.feed("#print axioms ax3")


def describe_入力の判定の途中でエンジンが異常終了する():
    """
    完結判定もエンジンとの通信なので、そこでエンジンが異常終了することがある。guard が
    エンジンを再起動すると証明モードは終了するが、その行の処理はまだ続いている。
    """

    @story("F1", "E1")
    def it_タクティクの行を宣言として送らない(repl, mocker):
        repl.feed("theorem probeDied : 1 = 1 := by sorry")
        repl.feed(":prove")

        real = leani.engine.Engine.query
        dead = []

        def die_once(self, src, **kw):
            if not dead:
                dead.append(src)
                raise leani.types.EngineDied()
            return real(self, src, **kw)

        mocker.patch.object(leani.engine.Engine, "query", die_once)
        out = repl.feed("rfl")
        mocker.stopall()

        # 証明モードが終了したあとに command として処理すると、タクティクが
        # 宣言として送られて "expected command" になる。
        assert "expected command" not in out, out
        assert "実行しなかった" in out, out
        # そのうえ submit_cmd の clear_pending が取得し直した sorry を消すので、
        # 直前に表示した「:prove で再開できる」が正しくなくなる。
        assert "再開できる" in out, out
        assert "sorry" in repl.declarations[-1], repl.declarations
        assert repl.feed(":prove").count("⊢") >= 1, "案内どおりに再開できない"


def describe_保留した宣言とエンジンの世代():
    """
    取り消した宣言の中身は env id を持つ。再起動したエンジンにその id は無い。
    """

    @story("F1", "C2")
    def it_無効になった_env_id_を設定し直さない(repl):
        repl.feed("def held := 1")
        repl.repl.undone = repl.repl.eng.pop_decl()  # 前の宣言を書き直している途中
        repl.feed(":restart")

        repl.repl.restore_undone()  # 書き直さずにやめた
        # 無効になった env id を設定すると repl は "Unknown environment." しか返さず、
        # 何を入力しても反応しない端末になる。テキストから再実行する。
        assert "2" in repl.feed("#eval held + 1")

    @story("F1")
    def it_無効な_env_id_を持っていたらそのことを報告する(repl):
        repl.repl.eng.env = 987654  # 無効になった世代の env id を持つ状態
        out = repl.feed("def afterGhost := 1")

        assert "environment" in out.lower(), out
        assert ":restart" in out, out


def describe_環境に無い宣言の扱い():
    """
    replay でエラーになった宣言と実行できなかった宣言は env に無い。テキストは保留
    しておくが、そのまま書き出すとエラーになるファイルができる。
    """

    @story("F1", "F2")
    def it_再実行に失敗した宣言を保留する(repl, tmp_path):
        path = tmp_path / "lib2.lean"
        path.write_text("def libB := 10\n")
        repl.feed(f":l {path}")
        repl.feed("def usesB := libB + 1")

        path.write_text("def other := 1\n")  # libB を消す
        out = repl.feed(":restart")

        assert "再実行に失敗した宣言" in out, out
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

        # 保留した宣言を書き出さないと、:save は成功を報告したのに入力した宣言が消える。
        # そのまま書くと lean でエラーになる。
        assert "-- def usesC := libC + 1" in text, text
        assert "def other := 1" in text, text

    @story("F2")
    def it_reset_したら保留も捨てる(repl, tmp_path):
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

        # import を書き戻さないと、:save は成功を報告するのにそのファイルは :l でも
        # lean でもエラーになる (Unknown identifier が並ぶ)。
        assert "import Lean.Elab.Frontend" in head, head
        assert "import Lean\n" in head, head


def describe_切り替えが成功したとき():

    @story("G5")
    def it_入力した宣言も新しい環境に追加される(repl):
        repl.feed("def carried := 42")
        out = repl.feed(":env wide")

        # 切り替えが成功したときも、新しいエンジンには入力した宣言が無い。
        # 再実行しないと、入力した宣言が何も表示されずに消える。
        assert "42" in repl.feed("#eval carried"), out


def describe_保留と環境の切り替え():
    """
    replay でエラーになった宣言は保留 (`Engine.unplayed`) に残る。環境を作り
    直す操作で何も表示せずに消すと、「保留にした」と表示した直後に
    消えることになる。
    """

    def _strand(repl, tmp_path, name):
        """保留を 1 件作る。読み込んだファイルから依存先を消す。"""
        lib = tmp_path / f"{name}.lean"
        lib.write_text(f"def {name}Dep := 1\n")
        repl.feed(f":l {lib}")
        repl.feed(f"def {name}Use := {name}Dep + 1")
        lib.write_text("def other := 9\n")
        repl.feed(":restart")
        assert repl.repl.eng.unplayed == [f"def {name}Use := {name}Dep + 1"]
        return lib

    @story("G5", "F2")
    def it_env_の切り替えでも保留を引き継ぐ(repl, tmp_path):
        _strand(repl, tmp_path, "envKeep")

        repl.feed(":env wide")
        assert repl.repl.eng.unplayed == ["def envKeepUse := envKeepDep + 1"]

    @story("B4", "F2")
    def it_l_が保留を捨てるなら件数を表示する(repl, tmp_path):
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

        # ヘッダに書くと repl はヘッダ全体を捨てて起動するので、:l でも lean でも
        # エラーになるファイルができる。それでも :save は成功を報告する。
        assert "import Lean" in head, head
        assert "NotAModule" not in head, head
        assert "読み込めなかった" not in repl.feed(f":l {out_path}")


def describe_sorry_の置き換えの途中でエンジンが異常終了する():

    @story("E3", "F1")
    def it_置き換えが成功したのに_sorry_のままと報告しない(repl, mocker):
        repl.feed("theorem died : True := by sorry")
        repl.feed(":prove")

        real = leani.engine.Engine.send_cmd
        hit = []

        def die_once(self, src, **kw):
            if "by trivial" in src and not hit:
                hit.append(src)
                raise leani.types.EngineDied()
            return real(self, src, **kw)

        mocker.patch.object(leani.engine.Engine, "send_cmd", die_once)
        out = repl.feed("trivial")
        mocker.stopall()

        # guard がエンジンを再起動して再送し、置き換えた宣言は成功している。世代だけを
        # 見て「sorry のまま」と報告すると、sorry のままのテキストを再実行して
        # 重複エラーになった宣言が保留にずっと残る (:restart のたびに
        # 「再実行に失敗した宣言」と表示される)。
        assert repl.declarations == ["theorem died : True := by trivial"]
        assert "sorry のままにしておく" not in out, out
        assert repl.repl.eng.unplayed == [], repl.repl.eng.unplayed


def describe_再起動に失敗したときの証明モード():

    @story("F1", "E1")
    def it_無効になった_proofState_を使い続けない(repl, mocker):
        repl.feed("theorem ghost : True := by sorry")
        repl.feed(":prove")

        real = leani.engine.Engine.query
        dead = []

        def die_once(self, src, **kw):
            if not dead:
                dead.append(src)
                raise leani.types.EngineDied()
            return real(self, src, **kw)

        mocker.patch.object(leani.engine.Engine, "query", die_once)
        mocker.patch.object(
            leani.engine.Engine, "boot", side_effect=leani.types.EngineDied()
        )
        out = repl.feed("trivial")

        # 証明モードを終了しないと、以降どの行にも赤い "Unknown proof state." だけを返す
        # 使えない証明モードのままになる (終了する方法の案内も表示されない)。
        assert "再起動できなかった" in out, out
        assert "証明モードを終了した" in out, out
        assert repl.repl.proof is None

        mocker.stopall()
        assert "1 件" in repl.feed(":restart")


def describe_元になる環境を失ったとき():
    """
    boot が失敗すると環境がどこにも無い。この状態で操作を受け付けても、事実と違う報告を
    せず、入力したテキストを失わないことだけは守る (原因を直せば :restart で戻る)。
    """

    def _no_env(repl, mocker):
        """boot が失敗するエンジンにする。保留に宣言 1 件を残す。"""
        repl.feed("def held := 1")
        mocker.patch.object(
            leani.engine.Engine, "boot", side_effect=leani.types.EngineDied()
        )
        repl.feed(":restart")
        assert repl.repl.eng.env is None
        assert repl.repl.eng.unplayed == ["def held := 1"]

    @story("F1", "F2")
    def it_reset_は受け付けずに保留を残す(repl, mocker):
        _no_env(repl, mocker)

        out = repl.feed(":reset")
        # base は終了したプロセスの id。設定し直すと submit の「env が無い」ガードが
        # 無効になり、import が 1 つも無い環境に宣言が追加される (何を書いても
        # エラーになる)。
        assert "元になる環境が無い" in out, out
        assert repl.repl.eng.unplayed == ["def held := 1"]

        mocker.stopall()
        assert "1 件" in repl.feed(":restart")
        assert "1" in repl.feed("#eval held")

    @story("F1")
    def it_再実行せずに保留する(repl, mocker):
        _no_env(repl, mocker)

        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            repl.repl.replay_into(["def two := 2"])
        out = buf.getvalue()

        # 実行すると repl は env 無しのリクエストから Init だけの環境を勝手に作る。
        # 「再実行した」と報告しながら、設定の import が無い環境に宣言を追加する
        # ことになる。
        assert "再実行できなかった" in out, out
        assert repl.repl.eng.unplayed == ["def two := 2", "def held := 1"]
        assert repl.repl.eng.log == []
        repl.check("replay_into のあと")

    @story("F1", "D2")
    def it_型も返さない(repl, mocker):
        _no_env(repl, mocker)

        # 答えると Init だけの環境からの答えになる。設定の import が無いので
        # 正しくない答えになるうえ、「エンジンは正常」に見えてしまう。
        assert "取得できなかった" in repl.feed(":t 1 + 1")
        assert repl.repl.eng.env is None

    @story("F1", "E1")
    def it_sorry_の持ち越しも捨てる(repl, mocker):
        repl.feed("theorem ghostSorry : True := by sorry")
        assert repl.repl.pending

        real = leani.engine.Engine.query
        dead = []

        def die_once(self, src, **kw):
            if not dead:
                dead.append(src)
                raise leani.types.EngineDied()
            return real(self, src, **kw)

        mocker.patch.object(leani.engine.Engine, "query", die_once)
        mocker.patch.object(
            leani.engine.Engine, "boot", side_effect=leani.types.EngineDied()
        )
        repl.feed("def afterGhost := 1")

        # 残すと :goals が環境に無い宣言のゴールを表示し、:prove がその無効な
        # proofState で証明モードを始める。
        assert repl.repl.pending == []
        assert "sorry" in repl.feed(":prove")  # 「sorry が無い」と表示するだけ
        assert repl.repl.proof is None

    @story("F1")
    def it_送る前にエラーにする(repl, mocker):
        _no_env(repl, mocker)
        eng = repl.repl.eng

        # 環境が無いときにどうするかを決めていない呼び出しは、ここでエラーになる。
        # env キーを付けずに送ると repl は Init だけの環境を作って答えるので、
        # 成功したように見える宣言が次のリクエストで消える。
        with pytest.raises(leani.types.NoEnvironment):
            eng.send_cmd("def sneaked := 1")
        assert eng.log == []

        # 新しい環境を作るための呼び出し (boot と :l) はそのまま送る。
        assert "env" in eng.send_cmd("#check Nat", fresh=True)


def describe_解決できない_import_のファイル():

    @story("B4", "G4")
    def it_読み込んだと表示しない(repl, tmp_path):
        path = tmp_path / "badimport.lean"
        # 本文は空にしておく。宣言があると、ヘッダを捨てられた環境でその宣言が
        # エラーになり、probe の確認より前に失敗してしまう。
        path.write_text("import NoSuchModuleXYZ\n\n-- 中身は無い\n")
        out = repl.feed(f":l {path}")

        # import が 1 つでも解決できないと repl はヘッダを丸ごと捨てて (エラーも
        # 出さずに) 環境を返す。boot と同じ確認をしないと、「読み込んだ」と
        # 報告したうえで、完結判定も補完も宣言も失敗する環境のままになる。
        assert "読み込めなかった" in out, out
        assert "import をすべて無視した" in out, out
        assert "2" in repl.feed("#eval 1 + 1"), "今の環境まで失った"


def describe_定理を外部サービスで探す():
    """:loogle は外部サービスに問い合わせるだけで、エンジンは一度も操作しない。"""

    @story("D4")
    def it_パターンが無ければ問い合わせない(repl, mocker):
        asked = mocker.patch.object(leani.repl, "loogle")
        assert "必要" in repl.feed(":loogle")
        assert not asked.called, "空の :loogle で問い合わせた"

    @story("D4")
    def it_問い合わせても環境は変わらない(repl, mocker):
        repl.feed("def beforeLoogle := 7")
        env = repl.engine.env
        hit = {
            "name": "Nat.add_comm",
            "type": " : ∀ (n m : Nat), n + m = m + n",
            "module": "Init",
        }
        mocker.patch.object(
            leani.repl, "loogle", return_value={"count": 1, "hits": [hit]}
        )

        assert "Nat.add_comm" in repl.feed(":loogle ?a + ?b = ?b + ?a")
        # loogle が返すのは Mathlib の名前で、今の環境とは無関係。
        assert repl.engine.env == env, ":loogle が環境を変えた"
        assert "7" in repl.feed("beforeLoogle"), ":loogle が宣言を消した"

    @story("D4", "F4")
    def it_loogle_が応答しなくてもセッションは続く(repl, mocker):
        err = leani.types.SearchError("timed out")
        mocker.patch.object(leani.repl, "loogle", side_effect=err)

        out = repl.feed(":loogle Nat")
        assert "検索に失敗した" in out, out
        assert "timed out" in out, out
        assert "2" in repl.feed("1 + 1"), "loogle の失敗でセッションが壊れた"
