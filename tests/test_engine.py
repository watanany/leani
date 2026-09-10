"""Repl を直接叩く層。端末は挟まないが、本物のエンジンと話す。

`repl` fixture は 1 行食わせるごとに不変条件 (conftest.INVARIANTS) を
確認するので、各テストの assert は「その操作で何が起きてほしいか」だけを
書けばいい。
"""
import leani


def describe_式の評価():

    def it_裸の式は評価されて値が出る(repl):
        assert "2" in repl.feed("1 + 1")

    def it_IO_の式はそのまま走る(repl):
        assert "hi" in repl.feed('IO.println "hi"')

    def it_評価できない項は型だけ出す(repl):
        # Nat -> Nat に Repr が無いので #eval できない。
        out = repl.feed("Nat.succ")
        assert "型だけ" in out, out


def describe_複数行の宣言():

    def it_空行までまとめて一つの宣言として通る(repl):
        repl.block("def fib : Nat -> Nat",
                   "  | 0 => 0",
                   "  | 1 => 1",
                   "  | n+2 => fib n + fib (n+1)")
        assert "55" in repl.feed("fib 10")
        assert len(repl.declarations) == 1

    def it_確定した直後のインデント行は前の入力に遡って続きになる(repl):
        repl.feed("def three := 1")
        repl.block("  + 2")
        assert "3" in repl.feed("three")
        assert len(repl.declarations) == 1, "遡らずに 2 件になった"

    def it_エラーになった宣言は環境を進めない(repl):
        repl.feed('def broken : Nat := "oops"')
        assert repl.declarations == []


def describe_証明モード():

    def it_exact_の提案を台本に入れる(repl):
        # `exact?` のままだと :save したファイルで毎回検索が走り、
        # 結果も環境次第で変わる。
        repl.feed("theorem t1 (n : Nat) : n + 0 = n := by sorry")
        repl.feed(":prove")
        out = repl.feed("exact?")          # 1 手で閉じるのでそのまま抜ける
        assert "台本には" in out, out
        assert "証明完了" in out, out
        assert "exact?" not in out[out.index("証明完了"):], "埋め戻しに残った"

    def it_複数手の証明で目標と台本を見られる(repl):
        repl.feed("theorem t2 : 1 = 1 ∧ 2 = 2 := by sorry")
        repl.feed(":prove")
        repl.feed("constructor")
        assert "1 = 1" in repl.feed(":goals")
        assert "constructor" in repl.feed(":script")
        repl.feed("rfl")
        assert "証明完了" in repl.feed("rfl")

    def it_証明モードの外で打っても困らせない(repl):
        # 証明が通ると自動で抜けるので、その直後に打たれることがある。
        assert "証明モードではない" in repl.feed(":done")
        assert "証明モードではない" in repl.feed(":goals")

    def it_undo_のあとの_prove_が手前の宣言を消さない(repl):
        repl.feed("def keepme := 42")
        repl.feed("theorem t0 : 1 = 1 := by sorry")
        repl.feed(":undo")
        repl.feed(":prove")
        assert "42" in repl.feed("keepme"), "手前の宣言が巻き戻された"
        assert len(repl.declarations) == 1


def describe_ファイルの読み書き():

    def it_save_したファイルを読み直せる(repl, tmp_path):
        path = tmp_path / "saved.lean"
        repl.feed("def saved := 7")
        assert str(path) in repl.feed(f":save {path}")
        assert "def saved := 7" in path.read_text()
        repl.feed(":reset")
        repl.feed(f":l {path}")
        assert "7" in repl.feed("saved")

    def it_読み込みに失敗しても手元の環境を失わない(repl, tmp_path):
        # env を捨てたまま戻さないと、以後の入力が import 無しの環境に飛んで
        # 何を書いても通らなくなる (不変条件「環境 id を持っている」で発見)。
        broken = tmp_path / "broken.lean"
        broken.write_text('def broken : Nat := "oops"\n')
        repl.feed("def before := 5")
        assert "読み込めなかった" in repl.feed(f":l {broken}")
        assert "5" in repl.feed("before")

    def it_読めないファイルは理由を出す(repl, tmp_path, mocker):
        path = tmp_path / "locked.lean"
        path.write_text("def x := 1\n")
        mocker.patch("leani.open", create=True,
                     side_effect=PermissionError(13, "Permission denied"))
        assert "読めない" in repl.feed(f":l {path}")


def describe_落ちても続く():

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

    def it_設定に無い名前を言われても続く(repl):
        assert "設定に無い" in repl.feed(":env nosuch")
        assert "2" in repl.feed("1 + 1"), "環境を壊して抜けている"
