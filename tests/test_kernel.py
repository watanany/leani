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
from conftest import SRC, leani, story
from jupyter_client.manager import start_new_kernel

import leani.kernel


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
            env=dict(os.environ, PYTHONPATH=SRC),
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

    @story("J1")
    def it_式の値を表示する(kernel):
        status, out, _ = run(kernel, "#eval 1 + 1")
        assert status == "ok"
        assert "2" in out, out

    @story("J1")
    def it_前のセルの宣言を使える(kernel):
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
    def it_エラーのセルは_status_が_error_になる(kernel):
        # status が error なら、「すべて実行」はそのセルで止まる。
        status, _, err = run(kernel, "#eval (1 : String)")
        assert status == "error"
        assert "error" in err.lower() or "failed" in err.lower(), err

    @story("J1")
    def it_silent_のセルは何も表示しない(kernel):
        _, kc = kernel
        msg_id = kc.execute("#eval 3 + 4", silent=True)
        streams = []
        while True:
            m = kc.get_iopub_msg(timeout=60)
            if m["parent_header"].get("msg_id") != msg_id:
                continue
            if m["msg_type"] == "stream":
                streams.append(m["content"]["text"])
            elif (
                m["msg_type"] == "status" and m["content"]["execution_state"] == "idle"
            ):
                break
        # reply を読まずに残すと、次の run が 1 つ前のセルの status を読む。
        assert kc.get_shell_msg(timeout=60)["content"]["status"] == "ok"
        assert streams == []

    @story("J1", "E3")
    def it_証明を完了すると_sorry_を置き換える(kernel):
        run(kernel, "theorem cell_t : 1 + 1 = 2 := by sorry")
        run(kernel, ":prove")
        status, _, err = run(kernel, "rfl")
        assert status == "ok", err
        _, out, _ = run(kernel, "#print axioms cell_t")
        assert "sorryAx" not in out, out

    @story("J1")
    def it_閉じていない複数行ブロックのセルはエラーになり次のセルは実行できる(
        kernel,
    ):
        status, out, err = run(kernel, ":{\ndef unclosed := 1")
        assert status == "error", out
        assert ":} が無い" in err, err
        status, out, _ = run(kernel, "#eval 2 + 3")
        assert status == "ok"
        assert "5" in out, out

    @story("J1")
    def it_q_はノートブックでは何もしない(kernel):
        _, out, _ = run(kernel, ":q")
        assert "ノートブックでは何もしない" in out, out
        _, out, _ = run(kernel, "#eval 3 + 3")
        assert "6" in out, out


def describe_補完():

    @story("J2", "D1")
    def it_名前を補完する(kernel):
        run(kernel, "#eval 0")  # エンジンを起動しておく
        head, matches = complete(kernel, "#check Nat.add_c")
        assert head == "Nat.add_c"
        assert "Nat.add_comm" in matches, matches

    @story("J2", "C5")
    def it_略記を記号に補完する(kernel):
        head, matches = complete(kernel, "theorem x : p \\to")
        assert head == "\\to"
        assert matches[0] == "→", matches


def describe_エンジンの異常終了と中断():

    @story("J3", "F1")
    def it_エンジンが異常終了しても宣言が残る(kernel):
        km, _ = kernel
        run(kernel, "def kept := 41")
        for pid in engine_pids(km):
            os.kill(pid, signal.SIGKILL)

        _, out, _ = run(kernel, "#eval kept + 1")
        assert "42" in out, out

    @story("J3", "A4")
    def it_中断しても宣言が残る(kernel, tmp_path):
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

    @pytest.fixture
    def direct(mocker):
        """LeaniKernel を直接作る。Repl と設定は差し替える。"""
        cfg = leani.config.EnvConfig.make("t")
        mocker.patch.object(leani.kernel, "resolve", return_value=cfg)
        mocker.patch.object(leani.kernel, "problem", return_value=None)
        fake = mocker.MagicMock(explicit=False, buf=[])
        fake.feed.return_value = None
        make = mocker.patch.object(leani.kernel, "Repl", return_value=fake)
        return leani.kernel.LeaniKernel(), make, fake

    @story("J1", "G4")
    def it_設定のエラーはセルに表示して次のセルでもう一度起動する(
        direct, mocker, capsys
    ):
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
    def it_エンジンを起動できなければセルが_error_になり次のセルで起動し直す(
        direct, capsys
    ):
        k, make, fake = direct
        make.side_effect = [leani.types.EngineDied("起動しない"), fake]
        assert execute(k) == "error"
        assert "起動しない" in capsys.readouterr().err
        assert execute(k) == "ok"
        assert make.call_count == 2

    @story("J3")
    @pytest.mark.parametrize(
        "exc", [leani.types.EngineError("再起動しない"), SystemExit(1)]
    )
    def it_再起動に失敗したら_repl_を捨てて次のセルで起動する(direct, capsys, exc):
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
    def it_想定外の例外は内部エラーとして表示しセッションを残す(direct, capsys):
        k, _, fake = direct
        execute(k)
        fake.feed.side_effect = ValueError("想定外")
        assert execute(k) == "error"
        assert "内部エラー: ValueError: 想定外" in capsys.readouterr().err
        assert k.repl is fake
        assert fake.discard.called


def describe_カーネルの登録():

    @story("J1")
    def it_今の_Python_で_leani_kernel_を起動するカーネルを登録する(tmp_path):
        env = dict(os.environ, JUPYTER_DATA_DIR=str(tmp_path), PYTHONPATH=SRC)
        subprocess.run(
            [sys.executable, "-m", "leani", "--install-kernel"], env=env, check=True
        )
        spec = json.loads((tmp_path / "kernels/leani/kernel.json").read_text())
        assert spec["argv"][:3] == [sys.executable, "-m", "leani.kernel"]
        assert spec["env"] == {"FORCE_COLOR": "1"}
