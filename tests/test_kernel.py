"""
Jupyter のカーネル。

多くのテストは jupyter_client で本物のカーネルを起動し、セルを送って確かめる。
カーネルは別のプロセスなので、Repl の状態は見えない。セルの出力と status だけで
確かめる。
起動や再起動の失敗は、別のプロセスでは起こせないので、LeaniKernel を直接作り、
Repl を差し替えて確かめる。
"""

import asyncio
import json
import os
import signal
import subprocess
import sys
import time

import pytest
from conftest import SRC
from jupyter_client.manager import start_new_kernel
from prompt_toolkit.completion import CompleteEvent
from prompt_toolkit.document import Document
from stories import story

import leani.boot
import leani.kernel
from leani.repl import NameCompleter


def kernel_env():
    # ipykernel は PYTEST_CURRENT_TEST があると、子プロセスの出力を取り込まない。
    # Jupyter で使うときと同じ動きにするため、この変数を渡さない。
    env = {k: v for k, v in os.environ.items() if k != "PYTEST_CURRENT_TEST"}
    return dict(env, PYTHONPATH=SRC)


@pytest.fixture(scope="module")
def kernel(tmp_path_factory):
    # テスト用の kernelspec を作り、JUPYTER_PATH でその場所を指定する。ユーザーの
    # Jupyter に登録したカーネルは使わない。
    root = tmp_path_factory.mktemp("jupyter")
    spec = root / "kernels" / "leani-test"
    spec.mkdir(parents=True)
    (spec / "kernel.json").write_text(
        json.dumps(
            {
                "argv": [
                    sys.executable,
                    "-m",
                    "leani.kernel",
                    "-f",
                    "{connection_file}",
                ],
                "display_name": "leani (test)",
                "language": "lean4",
            }
        )
    )
    with pytest.MonkeyPatch.context() as mp:
        mp.setenv("JUPYTER_PATH", str(root))
        km, kc = start_new_kernel(
            kernel_name="leani-test",
            cwd=str(tmp_path_factory.mktemp("cwd")),
            env=kernel_env(),
        )
    yield km, kc
    kc.stop_channels()
    km.shutdown_kernel(now=True)


def run(kernel, code, timeout=120):
    """セルを実行して (status, stdout, stderr) を返す。"""
    _, kc = kernel
    msg_id = kc.execute(code)
    out, err = [], []
    while True:
        m = kc.get_iopub_msg(timeout=timeout)
        if m["parent_header"].get("msg_id") != msg_id:
            continue
        c = m["content"]
        if m["msg_type"] == "stream":
            (out if c["name"] == "stdout" else err).append(c["text"])
        elif m["msg_type"] == "status" and c["execution_state"] == "idle":
            break

    reply = kc.get_shell_msg(timeout=timeout)
    return reply["content"]["status"], "".join(out), "".join(err)


def run_silent(kernel, code, timeout=120):
    """silent でセルを実行して (status, stream の文字列のリスト) を返す。"""
    _, kc = kernel
    msg_id = kc.execute(code, silent=True)
    streams = []
    while True:
        m = kc.get_iopub_msg(timeout=timeout)
        if m["parent_header"].get("msg_id") != msg_id:
            continue
        if m["msg_type"] == "stream":
            streams.append(m["content"]["text"])
        elif m["msg_type"] == "status" and m["content"]["execution_state"] == "idle":
            break
    # reply を読まずに残すと、次の run が 1 つ前のセルの status を読む。
    return kc.get_shell_msg(timeout=timeout)["content"]["status"], streams


def complete(kernel, code):
    _, kc = kernel
    kc.complete(code, len(code))
    reply = kc.get_shell_msg(timeout=60)["content"]
    return code[reply["cursor_start"] : reply["cursor_end"]], reply["matches"]


def engine_pids(km):
    """カーネルの子プロセス (エンジン) の pid。"""
    got = subprocess.run(
        ["pgrep", "-P", str(km.provisioner.process.pid)],
        capture_output=True,
        text=True,
        check=False,
    )
    return [int(p) for p in got.stdout.split()]


def describe_セルの実行():
    """
    ja: セルの実行
    """

    @story("J1")
    def 式のセルを実行すると値を表示する(kernel):
        """
        ja: 式のセルを実行すると値を表示する
        """
        status, out, _ = run(kernel, "#eval 1 + 1")
        assert status == "ok"
        assert "2" in out, out

    @story("J1")
    def 前のセルで宣言すると次のセルでその名前を使える(kernel):
        """
        ja: 前のセルで宣言した関数を次のセルで評価すると値を表示する
        """
        cell = """\
def fib : Nat → Nat
  | 0 => 0
  | 1 => 1
  | n + 2 => fib n + fib (n + 1)"""
        status, _, err = run(kernel, cell)
        assert status == "ok", err
        _, out, _ = run(kernel, "#eval fib 10")
        assert "55" in out, out

    @story("J1")
    def エラーになるセルを実行すると_status_が_error_になる(kernel):
        """
        ja: エラーになるセルを実行すると status が error になる
        """
        # status が error なら、「すべて実行」はそのセルで止まる。
        status, _, err = run(kernel, "#eval (1 : String)")
        assert status == "error"
        assert "error" in err.lower() or "failed" in err.lower(), err

    @story("J1")
    def silent_のセルを実行すると何も表示しない(kernel):
        """
        ja: silent のセルを実行すると、何も表示せずに ok を返す
        """
        status, streams = run_silent(kernel, "#eval 3 + 4")
        assert status == "ok"
        assert streams == []

    @story("J1")
    def 前のセルで起動したプロセスが出力しても_silent_のセルには表示しない(kernel):
        """
        ja: 前のセルで起動したプロセスが silent のセルの実行中に出力すると、何も表示せずに ok を返す
        """
        # 1 秒後に stderr に書くプロセスを起動し、その間に silent のセルを実行する。
        # カーネルが fd 2 の出力を取り込んでいると、その出力は実行中のセルに表示される。
        spawn = (
            "#eval do let _ ← IO.Process.spawn "
            '{ cmd := "sh", args := #["-c", "sleep 1; echo late >&2"], '
            "stderr := .inherit }"
        )
        assert run(kernel, spawn)[0] == "ok"
        status, streams = run_silent(kernel, "#eval (IO.sleep 3000 : IO Unit)")
        assert status == "ok"
        assert streams == []

    @story("J1", "E3")
    def 証明モードで証明を完了すると_sorry_を置き換える(kernel):
        """
        ja: 証明モードで証明を完了すると、定理の sorry を置き換える
        """
        run(kernel, "theorem cell_t : 1 + 1 = 2 := by sorry")
        run(kernel, ":prove")
        status, _, err = run(kernel, "rfl")
        assert status == "ok", err
        _, out, _ = run(kernel, "#print axioms cell_t")
        assert "sorryAx" not in out, out

    @story("J1", "E1")
    def 証明モードで閉じていないタクティクを入力すると次の行に続ける(kernel):
        """
        ja: 証明モードで括弧を閉じていないタクティクを送ると、次の行と合わせて実行する
        """
        run(
            kernel,
            "theorem cell_pair (p q : Prop) (hp : p) (hq : q) : p ∧ q := by sorry",
        )
        run(kernel, ":prove")
        status, _, err = run(kernel, "exact ⟨hp,\nhq⟩")
        assert status == "ok", err
        _, out, _ = run(kernel, "#print axioms cell_pair")
        assert "sorryAx" not in out, out

    @story("J1")
    def 複数行の文字列を評価しても値は変わらない(kernel):
        """
        ja: 複数行の文字列を含む式のセルを実行すると、文字列を変えずに値を表示する
        """
        # #eval に包むときに 2 行目の先頭に空白を足すと、文字列の値が変わる。
        status, out, err = run(kernel, '"Hello\nWorld".length')
        assert status == "ok", err
        assert "11" in out, out

    @story("J1", "E3")
    def 複数行の文字列で証明しても_sorry_を置き換えた宣言の値は変わらない(kernel):
        """
        ja: 証明モードで複数行の文字列を exact で送ると、その文字列のまま sorry を置き換える
        """
        run(kernel, "def cell_greeting : String := by sorry")
        run(kernel, ":prove")
        status, _, err = run(kernel, 'exact "Hello\nWorld"')
        assert status == "ok", err
        _, out, _ = run(kernel, "#eval cell_greeting.length")
        assert "11" in out, out

    @story("J1", "E1")
    def タクティクの続きを書きかけてやめたらタクティクを戻す(kernel):
        """
        ja: 証明モードでタクティクの続きを書きかけたままコマンドを送ると、直前のタクティクを残す
        """
        # インデントした行は直前のタクティクの続きとして読み直すので、直前の
        # タクティクを一度取り消す。続きを書かずにやめたら、取り消しを戻す。
        run(kernel, "theorem cell_two : 1 = 1 ∧ 2 = 2 := by sorry")
        run(kernel, ":prove")
        _, out, _ = run(kernel, "constructor\n  <;> exact (\n:goals")
        goals = out[out.index("捨てた") :]
        assert "1 = 1" in goals, out
        assert "∧" not in goals, out
        status, _, err = run(kernel, "rfl\nrfl")
        assert status == "ok", err

    @story("J1", "E1")
    def セミコロンでタクティクを並べると_1_行で実行する(kernel):
        """
        ja: 証明モードでセミコロンで区切ったタクティクを送ると、すべて実行して証明を完了する
        """
        run(kernel, "theorem cell_semi : 1 = 1 ∧ 2 = 2 := by sorry")
        run(kernel, ":prove")
        _, out, err = run(kernel, "constructor; rfl; rfl")
        assert "証明完了" in out, out + err

    @story("J1", "E1")
    def 読めないタクティクを送ると入力したとおりの位置でパースエラーを表示する(kernel):
        """
        ja: 証明モードで読めないタクティクを送ると、入力の中の位置でパースエラーを表示する
        """
        run(kernel, "theorem cell_paren : 1 = 1 := by sorry")
        run(kernel, ":prove")
        _, out, err = run(kernel, "rfl rfl")
        assert "<input>:1:4: expected end of input" in out + err, out + err
        _, out, err = run(kernel, "constructor )")
        assert "<input>:1:12" in out + err, out + err

    @story("J1")
    def 閉じていない_raw_文字列を入力すると次の行に続ける(kernel):
        """
        ja: 2 行にわたる raw 文字列を含むセルを実行すると、改行を含む 1 つの文字列として評価する
        """
        status, out, err = run(kernel, '#eval r#"a\nb"#.length')
        assert status == "ok", err
        assert "3" in out, out

    @story("J1")
    def 複数行ブロックを閉じずにセルを実行するとエラーになり次のセルは実行できる(
        kernel,
    ):
        """
        ja: 複数行のブロックを閉じないセルを実行すると、:} が無いと表示して失敗し、次のセルを実行する
        """
        status, out, err = run(kernel, ":{\ndef unclosed := 1")
        assert status == "error", out
        assert ":} が無い" in err, err
        status, out, _ = run(kernel, "#eval 2 + 3")
        assert status == "ok"
        assert "5" in out, out

    @story("J1")
    def colon_shell_を実行すると出力をセルに表示する(kernel):
        """
        ja: :! でコマンドを実行すると、その stdout と stderr をセルに表示する
        """
        status, out, err = run(kernel, ":! echo to-out; echo to-err >&2; exit 3")
        assert status == "ok"  # 終了コードは見ない (端末と同じ)
        assert out.endswith("to-out\n"), out
        assert err == "to-err\n", err

    @story("J1", "C7")
    def colon_abbrev_を実行すると一覧をセルに表示する(kernel):
        """
        ja: :abbrev に記号を付けて実行すると、その記号の略記をセルに表示する
        """
        status, out, _ = run(kernel, ":abbrev →")
        assert status == "ok"
        assert "\\to" in out, out

    @story("J1", "D4")
    def パターンを付けずに_colon_loogle_を実行すると_status_が_error_になる(kernel):
        """
        ja: パターンを付けずに :loogle を実行すると、パターンが必要と表示して失敗する
        """
        # 引数を確かめたところで止まるので、loogle には問い合わせない。
        status, _, err = run(kernel, ":loogle")
        assert status == "error"
        assert "パターンが必要" in err, err

    @story("J1")
    def ノートブックで_colon_q_を実行しても終了しない(kernel):
        """
        ja: :q を実行すると、ノートブックでは何もしないと表示し、次のセルを実行する
        """
        _, out, _ = run(kernel, ":q")
        assert "ノートブックでは何もしない" in out, out
        _, out, _ = run(kernel, "#eval 3 + 3")
        assert "6" in out, out

    @story("J1")
    def 改ページを含む文字列を入力しても式として評価する(kernel):
        """
        ja: 改ページの文字を含む文字列の式のセルを実行すると、文字列を変えずに式として評価する
        """
        # 完結の判定に送る文字列リテラルに \f と書くと、Lean が読めずに判定が
        # エラーになり、式を宣言として送ってしまう。#eval に包むときに \f を行の
        # 区切りと見なすと、文字列の中にインデントが入る。
        status, out, err = run(kernel, '"a\fb".length')
        assert status == "ok", err
        assert "3" in out.split(), out


def describe_補完():
    """
    ja: 補完
    """

    @story("J2", "D1")
    def 名前の途中で補完すると定数名を候補に出す(kernel):
        """
        ja: 名前の途中で補完すると、定数名を候補に出す
        """
        run(kernel, "#eval 0")  # エンジンを起動しておく
        head, matches = complete(kernel, "#check Nat.add_c")
        assert head == "Nat.add_c"
        assert "Nat.add_comm" in matches, matches

    @story("J2", "D1")
    def colon_undo_で宣言を取り消すとその名前は候補に出ない(kernel):
        """
        ja: :undo で宣言を取り消すと、その名前を候補から外す
        """
        run(kernel, "def undoneOnly := 1")
        assert "undoneOnly" in complete(kernel, "#check undoneO")[1]
        run(kernel, ":undo")
        _, matches = complete(kernel, "#check undoneO")
        assert "undoneOnly" not in matches, matches

    @story("J2", "D1")
    def colon_undo_のあとに同じ数の宣言を足しても取り消した名前は候補に出ない(
        kernel,
    ):
        """
        ja: :undo のあとに宣言を 1 つ足すと、足した名前を候補に出し、取り消した名前は出さない
        """
        # 宣言の数だけを見ると、取り消す前の名前を使い続ける。
        run(kernel, "def swappedA := 1")
        assert "swappedA" in complete(kernel, "#check swapped")[1]
        run(kernel, ":undo")
        run(kernel, "def swappedB := 2")
        _, matches = complete(kernel, "#check swapped")
        assert "swappedB" in matches, matches
        assert "swappedA" not in matches, matches

    @story("J2", "C5")
    def 略記の途中で補完すると記号を候補に出す(kernel):
        """
        ja: 略記を補完すると、その記号を最初の候補に出す
        """
        head, matches = complete(kernel, "theorem x : p \\to")
        assert head == "\\to"
        assert matches[0] == "→", matches

    @story("J2", "H1")
    def コマンド名の途中で補完するとコマンド名を候補に出す(kernel):
        """
        ja: コマンド名の途中で補完すると、入力した部分で始まるコマンド名を候補に出す
        """
        head, matches = complete(kernel, ":re")
        assert head == "re"
        assert matches == ["reload", "reset", "restart"], matches

    @story("J2", "B4")
    def セルの_2_行目の_colon_l_の後で補完するとファイルのパスを候補に出す(kernel):
        """
        ja: セルの 2 行目の :l の後ろを補完すると、.lean のファイルだけを候補に出す
        """
        run(
            kernel, ":! touch cell.lean cell.md"
        )  # カーネルのカレントディレクトリに作る
        head, matches = complete(kernel, "#eval 1\n:l cell.")
        assert head == "cell."
        assert matches == ["cell.lean"], matches


def jupyter(text):
    """NameCompleter の候補を Jupyter の形に直したもの。定数名の候補は常に Nat.succ。"""
    got = NameCompleter(lambda _: ["Nat.succ"]).get_completions(
        Document(text), CompleteEvent()
    )
    return leani.kernel.jupyter_matches(text, list(got))


def describe_Jupyter_の補完の形():
    """
    ja: Jupyter の補完の形
    """

    @story("J2", "B5")
    @pytest.mark.usefixtures("place")
    def 続きの文字だけの候補は_Jupyter_に返すときに語全体に直す():
        """
        ja: 続きの文字だけの候補は、入力済みの部分を含めた語全体に直す
        """
        assert jupyter(":! leanf") == (len(":! "), ["leanfoo"])
        assert jupyter(":! cat bin/leanf") == (len(":! cat bin/"), ["leanfoo"])

    @story("J2", "B4")
    @pytest.mark.usefixtures("place")
    def ディレクトリの候補には_slash_を付ける():
        """
        ja: ディレクトリの候補の後ろに / を付ける
        """
        assert jupyter(":l ") == (len(":l "), ["bin/", "notes.lean"])

    @story("J2", "D1")
    def 定数名の候補は名前全体を置き換える():
        """
        ja: 定数名の候補は、入力した名前全体を置き換える
        """
        assert jupyter("#check Nat.su") == (len("#check "), ["Nat.succ"])

    @story("J2", "D1")
    def 文字列のエスケープの後ろで補完すると名前を候補に出す():
        """
        ja: 文字列のエスケープの後ろにある名前だけを定数名の候補で置き換える
        """
        # `\n",Nat.su` で始まる略記は無いので、`\` から後ろは略記ではない。
        text = '#eval ("a\\n",Nat.su'
        assert jupyter(text) == (len(text) - len("Nat.su"), ["Nat.succ"])

    @story("J2", "D1")
    def 名前の前に記号があれば記号の後ろから名前を補完する():
        """
        ja: 名前の前に記号があると、記号の後ろだけを定数名の候補で置き換える
        """
        assert jupyter("#check @Nat.su") == (len("#check @"), ["Nat.succ"])

    @story("J2", "D1")
    def 名前の前に_Lean_の名前に使えない文字があればその後ろから名前を補完する():
        """
        ja: 名前の前に Lean の名前に使えない文字があると、その文字の後ろだけを定数名の候補で置き換える
        """
        assert jupyter("#check 定理Nat.su") == (len("#check 定理"), ["Nat.succ"])

    @story("J2")
    def 候補が無ければカーソルの位置を返す():
        """
        ja: 候補が無ければ、カーソルの位置と空の一覧を返す
        """
        assert jupyter(":env ") == (len(":env "), [])


def describe_エンジンの異常終了と中断():
    """
    ja: エンジンの異常終了と中断
    """

    @story("J3", "F1")
    def エンジンが異常終了しても宣言が残る(kernel):
        """
        ja: エンジンが異常終了すると、それまでの宣言を残して次のセルを実行する
        """
        km, _ = kernel
        run(kernel, "def kept := 41")
        for pid in engine_pids(km):
            os.kill(pid, signal.SIGKILL)

        _, out, _ = run(kernel, "#eval kept + 1")
        assert "42" in out, out

    @story("J3", "A4")
    def 評価を中断しても宣言が残る(kernel, tmp_path):
        """
        ja: 評価の途中で中断すると、評価を止めてエラーにし、それまでの宣言を残す
        """
        km, kc = kernel
        run(kernel, "def before := 5")
        # 評価が始まってから中断する。評価の前 (完結判定の途中など) に中断すると、
        # 確かめたいエンジンの中断にならない。
        mark = tmp_path / "started"
        msg_id = kc.execute(f'#eval do IO.FS.writeFile "{mark}" ""; IO.sleep 60000')
        t0 = time.time()
        while not mark.exists():
            assert time.time() - t0 < 30, "評価が始まらない"
            time.sleep(0.1)
        km.interrupt_kernel()
        while True:
            m = kc.get_iopub_msg(timeout=60)
            if (
                m["parent_header"].get("msg_id") == msg_id
                and m["msg_type"] == "status"
                and m["content"]["execution_state"] == "idle"
            ):
                break
        assert time.time() - t0 < 30, "中断しても止まらず、評価が最後まで実行された"
        assert kc.get_shell_msg(timeout=60)["content"]["status"] == "error"

        _, out, _ = run(kernel, "#eval before")
        assert "5" in out, out


def execute(k, code="1"):
    """LeaniKernel にセルを 1 つ実行させて、status を返す。"""
    return asyncio.run(k.do_execute(code, False))["status"]


def describe_起動と再起動の失敗():
    """
    ja: 起動と再起動の失敗
    """

    @pytest.fixture
    def direct(mocker):
        """LeaniKernel を直接作る。Repl と設定は差し替える。"""
        cfg = leani.config.EnvConfig.make("t")
        mocker.patch.object(leani.kernel, "resolve", return_value=cfg)
        mocker.patch.object(leani.kernel, "problem", return_value=None)
        fake = mocker.MagicMock()
        fake.feed.return_value = None
        make = mocker.patch.object(leani.kernel, "Repl", return_value=fake)
        return leani.kernel.LeaniKernel(), make, fake

    @story("J1", "G4")
    def 設定にエラーがあればセルに表示して次のセルでもう一度起動する(
        direct, mocker, capsys
    ):
        """
        ja: 設定にエラーがあると、エラーを表示してセルを失敗させ、次のセルで起動をやり直す
        """
        k, make, _ = direct
        mocker.patch.object(
            leani.kernel, "resolve", side_effect=leani.types.ConfigError("壊れた設定")
        )
        assert execute(k) == "error"
        assert "壊れた設定" in capsys.readouterr().err
        assert k.repl is None

        mocker.patch.object(
            leani.kernel, "resolve", return_value=leani.config.EnvConfig.make("t")
        )
        assert execute(k) == "ok"
        assert make.called

    @story("J3")
    def エンジンを起動できなければセルが_error_になり次のセルで起動し直す(
        direct, capsys
    ):
        """
        ja: エンジンが起動しないと、エラーを表示してセルを失敗させ、次のセルで起動をやり直す
        """
        k, make, fake = direct
        make.side_effect = [leani.types.EngineDied("起動しない"), fake]
        assert execute(k) == "error"
        assert "起動しない" in capsys.readouterr().err
        assert execute(k) == "ok"
        assert make.call_count == 2

    @story("J3")
    @pytest.mark.parametrize(
        "exc",
        [
            leani.types.EngineError("再起動しない"),
            leani.types.EnvLost("main にも戻れなくなった"),
        ],
    )
    def 再起動に失敗したら_repl_を捨てて次のセルで起動する(direct, capsys, exc):
        """
        ja: エンジンの再起動に失敗すると、セルを失敗させ、次のセルでエンジンを起動し直す
        """
        k, make, fake = direct
        execute(k)
        fake.feed.side_effect = exc
        assert execute(k) == "error"
        assert "次のセルでエンジンを起動する" in capsys.readouterr().err
        assert k.repl is None

        fake.feed.side_effect = None
        assert execute(k) == "ok"
        assert make.call_count == 2

    @story("J3")
    def 想定外の例外が起きたら内部エラーとして表示しセッションを残す(direct, capsys):
        """
        ja: 想定外の例外が起きると、内部エラーと表示してセルを失敗させ、セッションを残す
        """
        k, _, fake = direct
        execute(k)
        fake.feed.side_effect = ValueError("想定外")
        assert execute(k) == "error"
        assert "内部エラー: ValueError: 想定外" in capsys.readouterr().err
        assert k.repl is fake
        assert fake.discard.called


def describe_Lean_のバージョン():
    """
    ja: Lean のバージョン
    """
    # 起動の用意も別のプロセスでは確かめられないので、lean を差し替えて boot を
    # 直接呼ぶ。

    @pytest.fixture
    def lean(mocker):
        """`lean --version` の差し替え。呼んだ引数を見られるようにする。"""
        run = mocker.patch.object(leani.boot.subprocess, "run")
        run.return_value.stdout = "Lean (version 4.34.0, arm64-apple-darwin, Release)\n"
        return run

    @story("J1", "G3")
    def lean_の_version_を調べるときは時間を区切らずに待つ(lean):
        """
        ja: 手元の toolchain を調べると、時間を区切らずに lean を実行し、lean が表示したバージョンを返す
        """
        assert leani.boot.local_toolchain() == "leanprover/lean4:v4.34.0"
        assert "timeout" not in lean.call_args.kwargs

    @story("J1", "G3")
    def toolchain_が_stable_なら今のバージョンに置き換える(lean, tmp_path):
        """
        ja: プロジェクトの toolchain が stable なら、stable の lean が示すバージョンに置き換える
        """
        (tmp_path / "lean-toolchain").write_text("leanprover/lean4:stable\n")
        cfg = leani.config.EnvConfig.make("t", project=str(tmp_path))
        assert leani.boot.guess_toolchain(cfg) == "leanprover/lean4:v4.34.0"
        assert lean.call_args.args[0] == [
            "lean",
            "+leanprover/lean4:stable",
            "--version",
        ]

    @story("J1", "G3")
    def エンジンを自分で指定したら_stable_を置き換えない(lean, tmp_path):
        """
        ja: 設定でエンジンを指定してあれば、stable をそのまま返し、lean を実行しない
        """
        (tmp_path / "lean-toolchain").write_text("leanprover/lean4:stable\n")
        cfg = leani.config.EnvConfig.make("t", engine=str(tmp_path))
        assert leani.boot.guess_toolchain(cfg) == "leanprover/lean4:stable"
        lean.assert_not_called()


def describe_カーネルの登録():
    """
    ja: カーネルの登録
    """

    @story("J1")
    def カーネルを登録すると今の_Python_で_leani_kernel_を起動する設定を書く(tmp_path):
        """
        ja: install を実行すると、今の Python で leani.kernel を起動する kernel.json を書く
        """
        env = dict(os.environ, JUPYTER_DATA_DIR=str(tmp_path), PYTHONPATH=SRC)
        subprocess.run(
            [sys.executable, "-m", "leani.kernel", "install"], env=env, check=True
        )
        spec = json.loads((tmp_path / "kernels/leani/kernel.json").read_text())
        assert spec["argv"][:3] == [sys.executable, "-m", "leani.kernel"]
        assert spec["env"] == {"FORCE_COLOR": "1"}


def describe_SPEC_md_との対応():
    """
    ja: SPEC との対応
    """

    @story("X1")
    def SPEC_md_に載らない形のテストを実行するとエラーにする(pytester):
        """
        ja: SPEC に載らない形のテストがあると、pytest の実行を失敗させ、そのテストの名前だけを表示する
        """
        pytester.makepyfile(
            test_forms="""
            from stories import story

            def describe_セル():
                @story("J1")
                def 載る():
                    pass

                def _補助():
                    pass

                def story_が無い():
                    pass

            def test_直下():
                pass

            class TestX:
                def test_中(self):
                    pass
            """
        )
        result = pytester.runpytest("-p", "stories", "-p", "no:xdist")
        out = result.stdout.str() + result.stderr.str()
        assert result.ret != 0
        for name in ("story_が無い", "test_直下", "test_中"):
            assert name in out
        for name in ("載る", "_補助"):
            assert name not in out
