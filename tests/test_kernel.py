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


def describe_running_cells():
    """
    ja: セルの実行
    en: Running cells
    """

    @story("J1")
    def shows_the_value_when_a_cell_with_an_expression_runs(kernel):
        """
        ja: 式のセルを実行すると値を表示する
        en: Shows the value when a cell with an expression runs
        """
        status, out, _ = run(kernel, "#eval 1 + 1")
        assert status == "ok"
        assert "2" in out, out

    @story("J1")
    def shows_the_value_when_a_cell_evaluates_a_function_declared_in_the_previous_cell(
        kernel,
    ):
        """
        ja: 前のセルで宣言した関数を次のセルで評価すると値を表示する
        en: Shows the value when a cell evaluates a function declared in the previous cell
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
    def sets_the_status_to_error_when_a_cell_with_an_error_runs(kernel):
        """
        ja: エラーになるセルを実行すると status が error になる
        en: Sets the status to error when a cell with an error runs
        """
        # status が error なら、「すべて実行」はそのセルで止まる。
        status, _, err = run(kernel, "#eval (1 : String)")
        assert status == "error"
        assert "error" in err.lower() or "failed" in err.lower(), err

    @story("J1")
    def returns_ok_without_showing_anything_when_a_silent_cell_runs(kernel):
        """
        ja: silent のセルを実行すると、何も表示せずに ok を返す
        en: Returns ok without showing anything when a silent cell runs
        """
        status, streams = run_silent(kernel, "#eval 3 + 4")
        assert status == "ok"
        assert streams == []

    @story("J1")
    def returns_ok_without_showing_anything_when_a_process_from_an_earlier_cell_writes_during_a_silent_cell(
        kernel,
    ):
        """
        ja: 前のセルで起動したプロセスが silent のセルの実行中に出力すると、何も表示せずに ok を返す
        en: Returns ok without showing anything when a process from an earlier cell writes during a silent cell
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
    def replaces_the_sorry_in_the_theorem_when_the_proof_is_finished_in_proof_mode(
        kernel,
    ):
        """
        ja: 証明モードで証明を完了すると、定理の sorry を置き換える
        en: Replaces the sorry in the theorem when the proof is finished in proof mode
        """
        run(kernel, "theorem cell_t : 1 + 1 = 2 := by sorry")
        run(kernel, ":prove")
        status, _, err = run(kernel, "rfl")
        assert status == "ok", err
        _, out, _ = run(kernel, "#print axioms cell_t")
        assert "sorryAx" not in out, out

    @story("J1", "E1")
    def runs_a_tactic_together_with_the_next_line_when_its_bracket_is_not_closed_in_proof_mode(
        kernel,
    ):
        """
        ja: 証明モードで括弧を閉じていないタクティクを送ると、次の行と合わせて実行する
        en: Runs a tactic together with the next line when its bracket is not closed in proof mode
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
    def shows_the_value_without_changing_the_string_when_a_cell_with_a_multi_line_string_runs(
        kernel,
    ):
        """
        ja: 複数行の文字列を含む式のセルを実行すると、文字列を変えずに値を表示する
        en: Shows the value without changing the string when a cell with a multi-line string runs
        """
        # #eval に包むときに 2 行目の先頭に空白を足すと、文字列の値が変わる。
        status, out, err = run(kernel, '"Hello\nWorld".length')
        assert status == "ok", err
        assert "11" in out, out

    @story("J1", "E3")
    def replaces_the_sorry_with_the_multi_line_string_unchanged_when_it_is_sent_with_exact_in_proof_mode(
        kernel,
    ):
        """
        ja: 証明モードで複数行の文字列を exact で送ると、その文字列のまま sorry を置き換える
        en: Replaces the sorry with the multi-line string unchanged when it is sent with exact in proof mode
        """
        run(kernel, "def cell_greeting : String := by sorry")
        run(kernel, ":prove")
        status, _, err = run(kernel, 'exact "Hello\nWorld"')
        assert status == "ok", err
        _, out, _ = run(kernel, "#eval cell_greeting.length")
        assert "11" in out, out

    @story("J1", "E1")
    def keeps_the_previous_tactic_when_a_command_is_sent_before_its_continuation_is_finished_in_proof_mode(
        kernel,
    ):
        """
        ja: 証明モードでタクティクの続きを書きかけたままコマンドを送ると、直前のタクティクを残す
        en: Keeps the previous tactic when a command is sent before its continuation is finished in proof mode
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
    def runs_every_tactic_and_completes_the_proof_when_tactics_separated_by_semicolons_are_sent_in_proof_mode(
        kernel,
    ):
        """
        ja: 証明モードでセミコロンで区切ったタクティクを送ると、すべて実行して証明を完了する
        en: Runs every tactic and completes the proof when tactics separated by semicolons are sent in proof mode
        """
        run(kernel, "theorem cell_semi : 1 = 1 ∧ 2 = 2 := by sorry")
        run(kernel, ":prove")
        _, out, err = run(kernel, "constructor; rfl; rfl")
        assert "証明完了" in out, out + err

    @story("J1", "E1")
    def shows_a_parse_error_at_the_position_in_the_input_when_an_unparsable_tactic_is_sent_in_proof_mode(
        kernel,
    ):
        """
        ja: 証明モードで読めないタクティクを送ると、入力の中の位置でパースエラーを表示する
        en: Shows a parse error at the position in the input when an unparsable tactic is sent in proof mode
        """
        run(kernel, "theorem cell_paren : 1 = 1 := by sorry")
        run(kernel, ":prove")
        _, out, err = run(kernel, "rfl rfl")
        assert "<input>:1:4: expected end of input" in out + err, out + err
        _, out, err = run(kernel, "constructor )")
        assert "<input>:1:12" in out + err, out + err

    @story("J1")
    def evaluates_a_raw_string_across_two_lines_as_one_string_with_a_newline_when_its_cell_runs(
        kernel,
    ):
        """
        ja: 2 行にわたる raw 文字列を含むセルを実行すると、改行を含む 1 つの文字列として評価する
        en: Evaluates a raw string across two lines as one string with a newline when its cell runs
        """
        status, out, err = run(kernel, '#eval r#"a\nb"#.length')
        assert status == "ok", err
        assert "3" in out, out

    @story("J1")
    def fails_showing_is_missing_and_runs_the_next_cell_when_a_cell_leaves_a_multi_line_block_open(
        kernel,
    ):
        """
        ja: 複数行のブロックを閉じないセルを実行すると、:} が無いと表示して失敗し、次のセルを実行する
        en: Fails showing :} is missing and runs the next cell when a cell leaves a multi-line block open
        """
        status, out, err = run(kernel, ":{\ndef unclosed := 1")
        assert status == "error", out
        assert ":} が無い" in err, err
        status, out, _ = run(kernel, "#eval 2 + 3")
        assert status == "ok"
        assert "5" in out, out

    @story("J1")
    def shows_stdout_and_stderr_in_the_cell_when_a_command_runs_with(kernel):
        """
        ja: :! でコマンドを実行すると、その stdout と stderr をセルに表示する
        en: Shows stdout and stderr in the cell when a command runs with :!
        """
        status, out, err = run(kernel, ":! echo to-out; echo to-err >&2; exit 3")
        assert status == "ok"  # 終了コードは見ない (端末と同じ)
        assert out.endswith("to-out\n"), out
        assert err == "to-err\n", err

    @story("J1", "C7")
    def shows_the_abbreviations_for_a_symbol_in_the_cell_when_abbrev_runs_with_it(
        kernel,
    ):
        """
        ja: :abbrev に記号を付けて実行すると、その記号の略記をセルに表示する
        en: Shows the abbreviations for a symbol in the cell when :abbrev runs with it
        """
        status, out, _ = run(kernel, ":abbrev →")
        assert status == "ok"
        assert "\\to" in out, out

    @story("J1", "D4")
    def fails_showing_a_pattern_is_needed_when_loogle_runs_without_a_pattern(kernel):
        """
        ja: パターンを付けずに :loogle を実行すると、パターンが必要と表示して失敗する
        en: Fails showing a pattern is needed when :loogle runs without a pattern
        """
        # 引数を確かめたところで止まるので、loogle には問い合わせない。
        status, _, err = run(kernel, ":loogle")
        assert status == "error"
        assert "パターンが必要" in err, err

    @story("J1")
    def shows_that_it_does_nothing_in_a_notebook_and_runs_the_next_cell_when_q_runs(
        kernel,
    ):
        """
        ja: :q を実行すると、ノートブックでは何もしないと表示し、次のセルを実行する
        en: Shows that it does nothing in a notebook and runs the next cell when :q runs
        """
        _, out, _ = run(kernel, ":q")
        assert "ノートブックでは何もしない" in out, out
        _, out, _ = run(kernel, "#eval 3 + 3")
        assert "6" in out, out

    @story("J1")
    def evaluates_the_expression_without_changing_the_string_when_a_cell_has_a_string_with_a_form_feed(
        kernel,
    ):
        """
        ja: 改ページの文字を含む文字列の式のセルを実行すると、文字列を変えずに式として評価する
        en: Evaluates the expression without changing the string when a cell has a string with a form feed
        """
        # 完結の判定に送る文字列リテラルに \f と書くと、Lean が読めずに判定が
        # エラーになり、式を宣言として送ってしまう。#eval に包むときに \f を行の
        # 区切りと見なすと、文字列の中にインデントが入る。
        status, out, err = run(kernel, '"a\fb".length')
        assert status == "ok", err
        assert "3" in out.split(), out


def describe_completion():
    """
    ja: 補完
    en: Completion
    """

    @story("J2", "D1")
    def offers_constant_names_as_candidates_when_a_partial_name_is_completed(kernel):
        """
        ja: 名前の途中で補完すると、定数名を候補に出す
        en: Offers constant names as candidates when a partial name is completed
        """
        run(kernel, "#eval 0")  # エンジンを起動しておく
        head, matches = complete(kernel, "#check Nat.add_c")
        assert head == "Nat.add_c"
        assert "Nat.add_comm" in matches, matches

    @story("J2", "D1")
    def removes_the_name_from_the_candidates_when_undo_undoes_its_declaration(kernel):
        """
        ja: :undo で宣言を取り消すと、その名前を候補から外す
        en: Removes the name from the candidates when :undo undoes its declaration
        """
        run(kernel, "def undoneOnly := 1")
        assert "undoneOnly" in complete(kernel, "#check undoneO")[1]
        run(kernel, ":undo")
        _, matches = complete(kernel, "#check undoneO")
        assert "undoneOnly" not in matches, matches

    @story("J2", "D1")
    def offers_the_added_name_as_a_candidate_but_not_the_undone_one_when_a_declaration_is_added_after_undo(
        kernel,
    ):
        """
        ja: :undo のあとに宣言を 1 つ足すと、足した名前を候補に出し、取り消した名前は出さない
        en: Offers the added name as a candidate but not the undone one when a declaration is added after :undo
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
    def offers_the_symbol_as_the_first_candidate_when_an_abbreviation_is_completed(
        kernel,
    ):
        """
        ja: 略記を補完すると、その記号を最初の候補に出す
        en: Offers the symbol as the first candidate when an abbreviation is completed
        """
        head, matches = complete(kernel, "theorem x : p \\to")
        assert head == "\\to"
        assert matches[0] == "→", matches

    @story("J2", "H1")
    def offers_command_names_starting_with_the_typed_part_as_candidates_when_completing_a_command_name(
        kernel,
    ):
        """
        ja: コマンド名の途中で補完すると、入力した部分で始まるコマンド名を候補に出す
        en: Offers command names starting with the typed part as candidates when completing a command name
        """
        head, matches = complete(kernel, ":re")
        assert head == "re"
        assert matches == ["reload", "reset", "restart"], matches

    @story("J2", "B4")
    def offers_only_lean_files_as_candidates_when_the_path_after_l_on_a_cell_s_second_line_is_completed(
        kernel,
    ):
        """
        ja: セルの 2 行目の :l の後ろを補完すると、.lean のファイルだけを候補に出す
        en: Offers only .lean files as candidates when the path after :l on a cell's second line is completed
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


def describe_completions_in_the_jupyter_form():
    """
    ja: Jupyter の補完の形
    en: Completions in the Jupyter form
    """

    @story("J2", "B5")
    @pytest.mark.usefixtures("place")
    def turns_a_candidate_with_only_the_rest_of_a_word_into_the_whole_word():
        """
        ja: 続きの文字だけの候補は、入力済みの部分を含めた語全体に直す
        en: Turns a candidate with only the rest of a word into the whole word
        """
        assert jupyter(":! leanf") == (len(":! "), ["leanfoo"])
        assert jupyter(":! cat bin/leanf") == (len(":! cat bin/"), ["leanfoo"])

    @story("J2", "B4")
    @pytest.mark.usefixtures("place")
    def appends_a_slash_to_a_directory_candidate():
        """
        ja: ディレクトリの候補の後ろに / を付ける
        en: Appends a slash to a directory candidate
        """
        assert jupyter(":l ") == (len(":l "), ["bin/", "notes.lean"])

    @story("J2", "D1")
    def replaces_the_whole_typed_name_with_a_constant_name_candidate():
        """
        ja: 定数名の候補は、入力した名前全体を置き換える
        en: Replaces the whole typed name with a constant name candidate
        """
        assert jupyter("#check Nat.su") == (len("#check "), ["Nat.succ"])

    @story("J2", "D1")
    def replaces_only_the_name_after_an_escape_in_a_string_with_a_constant_name_candidate():
        """
        ja: 文字列のエスケープの後ろにある名前だけを定数名の候補で置き換える
        en: Replaces only the name after an escape in a string with a constant name candidate
        """
        # `\n",Nat.su` で始まる略記は無いので、`\` から後ろは略記ではない。
        text = '#eval ("a\\n",Nat.su'
        assert jupyter(text) == (len(text) - len("Nat.su"), ["Nat.succ"])

    @story("J2", "D1")
    def replaces_only_the_part_after_a_symbol_before_a_name_with_a_constant_name_candidate():
        """
        ja: 名前の前に記号があると、記号の後ろだけを定数名の候補で置き換える
        en: Replaces only the part after a symbol before a name with a constant name candidate
        """
        assert jupyter("#check @Nat.su") == (len("#check @"), ["Nat.succ"])

    @story("J2", "D1")
    def replaces_only_the_part_after_a_character_not_allowed_in_lean_names_with_a_constant_name_candidate():
        """
        ja: 名前の前に Lean の名前に使えない文字があると、その文字の後ろだけを定数名の候補で置き換える
        en: Replaces only the part after a character not allowed in Lean names with a constant name candidate
        """
        assert jupyter("#check 定理Nat.su") == (len("#check 定理"), ["Nat.succ"])

    @story("J2")
    def returns_the_cursor_position_and_an_empty_list_if_there_is_no_candidate():
        """
        ja: 候補が無ければ、カーソルの位置と空の一覧を返す
        en: Returns the cursor position and an empty list if there is no candidate
        """
        assert jupyter(":env ") == (len(":env "), [])


def describe_engine_crashes_and_interrupts():
    """
    ja: エンジンの異常終了と中断
    en: Engine crashes and interrupts
    """

    @story("J3", "F1")
    def keeps_earlier_declarations_for_the_next_cell_when_the_engine_crashes(kernel):
        """
        ja: エンジンが異常終了すると、それまでの宣言を残して次のセルを実行する
        en: Keeps earlier declarations for the next cell when the engine crashes
        """
        km, _ = kernel
        run(kernel, "def kept := 41")
        for pid in engine_pids(km):
            os.kill(pid, signal.SIGKILL)

        _, out, _ = run(kernel, "#eval kept + 1")
        assert "42" in out, out

    @story("J3", "A4")
    def stops_the_evaluation_with_an_error_and_keeps_earlier_declarations_when_it_is_interrupted(
        kernel, tmp_path
    ):
        """
        ja: 評価の途中で中断すると、評価を止めてエラーにし、それまでの宣言を残す
        en: Stops the evaluation with an error and keeps earlier declarations when it is interrupted
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


def describe_startup_and_restart_failures():
    """
    ja: 起動と再起動の失敗
    en: Startup and restart failures
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
    def fails_the_cell_with_the_error_and_retries_startup_in_the_next_cell_if_the_config_has_an_error(
        direct, mocker, capsys
    ):
        """
        ja: 設定にエラーがあると、エラーを表示してセルを失敗させ、次のセルで起動をやり直す
        en: Fails the cell with the error and retries startup in the next cell if the config has an error
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
    def fails_the_cell_with_the_error_and_retries_startup_in_the_next_cell_when_the_engine_does_not_start(
        direct, capsys
    ):
        """
        ja: エンジンが起動しないと、エラーを表示してセルを失敗させ、次のセルで起動をやり直す
        en: Fails the cell with the error and retries startup in the next cell when the engine does not start
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
    def fails_the_cell_and_starts_the_engine_again_in_the_next_cell_when_a_restart_fails_or_env_cannot_return_to_the_original_environment(
        direct, capsys, exc
    ):
        """
        ja: エンジンの再起動に失敗するか、:env で元の環境にも戻れなくなると、セルを失敗させ、次のセルでエンジンを起動し直す
        en: Fails the cell and starts the engine again in the next cell when a restart fails or :env cannot return to the original environment
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
    def fails_the_cell_with_an_internal_error_and_keeps_the_session_when_an_unexpected_exception_occurs(
        direct, capsys
    ):
        """
        ja: 想定外の例外が起きると、内部エラーと表示してセルを失敗させ、セッションを残す
        en: Fails the cell with an internal error and keeps the session when an unexpected exception occurs
        """
        k, _, fake = direct
        execute(k)
        fake.feed.side_effect = ValueError("想定外")
        assert execute(k) == "error"
        assert "内部エラー: ValueError: 想定外" in capsys.readouterr().err
        assert k.repl is fake
        assert fake.discard.called


def describe_lean_version():
    """
    ja: Lean のバージョン
    en: Lean version
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
    def runs_lean_without_a_timeout_and_returns_the_version_it_prints_when_looking_up_the_local_toolchain(
        lean,
    ):
        """
        ja: 手元の toolchain を調べると、時間を区切らずに lean を実行し、lean が表示したバージョンを返す
        en: Runs lean without a timeout and returns the version it prints when looking up the local toolchain
        """
        assert leani.boot.local_toolchain() == "leanprover/lean4:v4.34.0"
        assert "timeout" not in lean.call_args.kwargs

    @story("J1", "G3")
    def replaces_stable_with_the_version_that_the_stable_lean_reports_if_the_project_uses_stable(
        lean, tmp_path
    ):
        """
        ja: プロジェクトの toolchain が stable なら、stable の lean が示すバージョンに置き換える
        en: Replaces stable with the version that the stable lean reports if the project uses stable
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
    def returns_stable_as_it_is_without_running_lean_if_the_config_sets_the_engine(
        lean, tmp_path
    ):
        """
        ja: 設定でエンジンを指定してあれば、stable をそのまま返し、lean を実行しない
        en: Returns stable as it is without running lean if the config sets the engine
        """
        (tmp_path / "lean-toolchain").write_text("leanprover/lean4:stable\n")
        cfg = leani.config.EnvConfig.make("t", engine=str(tmp_path))
        assert leani.boot.guess_toolchain(cfg) == "leanprover/lean4:stable"
        lean.assert_not_called()


def describe_registering_the_kernel():
    """
    ja: カーネルの登録
    en: Registering the kernel
    """

    @story("J1")
    def writes_a_kernel_json_that_starts_leani_kernel_with_the_current_python_when_install_runs(
        tmp_path,
    ):
        """
        ja: install を実行すると、今の Python で leani.kernel を起動する kernel.json を書く
        en: Writes a kernel.json that starts leani.kernel with the current Python when install runs
        """
        env = dict(os.environ, JUPYTER_DATA_DIR=str(tmp_path), PYTHONPATH=SRC)
        subprocess.run(
            [sys.executable, "-m", "leani.kernel", "install"], env=env, check=True
        )
        spec = json.loads((tmp_path / "kernels/leani/kernel.json").read_text())
        assert spec["argv"][:3] == [sys.executable, "-m", "leani.kernel"]
        assert spec["env"] == {"FORCE_COLOR": "1"}


def describe_correspondence_with_spec():
    """
    ja: SPEC との対応
    en: Correspondence with SPEC
    """

    @story("X1")
    def fails_the_run_and_shows_only_the_names_of_tests_spec_cannot_list_when_pytest_finds_such_tests(
        pytester,
    ):
        """
        ja: SPEC に載らない形のテストがあると、pytest の実行を失敗させ、そのテストの名前だけを表示する
        en: Fails the run and shows only the names of tests SPEC cannot list when pytest finds such tests
        """
        pytester.makepyfile(
            test_forms="""
            from stories import story

            def describe_cell():
                @story("J1")
                def listed_in_spec():
                    pass

                def _helper():
                    pass

                def missing_story():
                    pass

            def test_top_level():
                pass

            class TestX:
                def test_in_class(self):
                    pass
            """
        )
        result = pytester.runpytest("-p", "stories", "-p", "no:xdist")
        out = result.stdout.str() + result.stderr.str()
        assert result.ret != 0
        for name in ("missing_story", "test_top_level", "test_in_class"):
            assert name in out
        for name in ("listed_in_spec", "_helper"):
            assert name not in out

    @story("X1")
    def fails_the_run_when_a_test_is_skipped_or_marked_as_expected_to_fail(pytester):
        """
        ja: skip したテストか xfail のテストがあると、そのテストを失敗にする
        en: Fails the run when a test is skipped or marked as expected to fail
        """
        pytester.makepyfile(
            test_skips="""
            import pytest
            from stories import story

            def describe_cell():
                @story("J1")
                def skips_inside():
                    pytest.skip("x")

                @story("J1")
                @pytest.mark.xfail
                def fails_as_expected():
                    assert False

                @story("J1")
                @pytest.mark.xfail
                def passes_unexpectedly():
                    pass
            """
        )
        result = pytester.runpytest("-p", "stories", "-p", "no:xdist")
        result.assert_outcomes(failed=3)
