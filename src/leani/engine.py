"""エンジン (副作用)。

repl のプロセスを 1 つ管理し、JSON のリクエストとレスポンスをやりとりする。
プロセスが異常終了したら再起動して replay する。"""

from __future__ import annotations

import codecs
import contextlib
import json
import os
import select
import signal
import subprocess
import sys
import textwrap
from collections.abc import Sequence
from typing import NamedTuple

from leani.boot import ensure_engine, lake_env, toolchain
from leani.config import EnvConfig, read_text
from leani.places import BOOT_PROBE, PROBE_IMPORT
from leani.pure import (
    error_text,
    first_response,
    has_error,
    has_import,
    head_line,
    import_lines,
    info_text,
    signal_name,
    sorries,
    strip_imports,
    yellow,
)
from leani.types import EngineDied, Interrupted, Json, NoEnvironment, Response, Sorry


class Undone(NamedTuple):
    """取り消した宣言 1 件。元に戻すのに必要な情報だけを持つ。"""

    env: int | None
    src: str | None
    gen: int  # 保存したときのプロセスの世代。プロセスを再起動すると env id は無効になる


class Loaded(NamedTuple):
    """load_file の結果。src は import を足したあとの、実際に送ったソース。"""

    resp: Response
    bad: bool
    src: str
    note: str | None = None  # init を再適用できなかったときの理由


class Replay(NamedTuple):
    """
    replay の結果。環境に戻せなかったものも全部含めて返す。

    件数だけを返すと、失敗した宣言も読み込み直せなかったファイルも、何も表示されずに
    消える。環境から無くなったものは必ず報告する。
    """

    done: list[str]  # 成功した宣言
    failed: list[str]  # 失敗した宣言
    skipped: list[str]  # 途中で打ち切ったので実行していない宣言
    notes: list[str]  # init や :l で起きたこと
    sorries: list[Sorry]  # 最後に成功した宣言に残っている sorry

    @property
    def dropped(self) -> bool:
        """環境から無くなったものがあるか。"""
        return bool(self.failed or self.skipped or self.notes)


class Engine:
    """repl のサブプロセス 1 つ。異常終了したら restart() で再起動する。"""

    def __init__(self, cfg: EnvConfig) -> None:
        self.cfg = cfg
        self.tc = toolchain(cfg)
        self.dir = ensure_engine(cfg.engine, self.tc)
        self._warn_toolchain()
        self.proc_env = self._proc_env()
        self.proc: subprocess.Popen[str] | None = None
        self.env: int | None = None  # 今の env id
        self.base: int | None = None  # 起動直後 / :l 直後の env id
        self.stack: list[int] = []  # :undo 用
        self.log: list[str] = []  # 受理した宣言。再起動時に replay する
        self.unplayed: list[str] = []  # 入力したが env に追加されていない宣言
        self.gen = 0  # プロセスの世代。proofState の持ち主の確認に使う
        self.loaded: str | None = None  # :l したファイル
        self.loaded_src: str | None = None  # その中身 (import 行は除く)
        self.loaded_imports: list[str] = []  # そのファイルが書いていた import
        self.init_src: str | None = None  # init で実行したソース
        self.spawn()

    # -- 環境変数 ---------------------------------------------------------

    def _warn_toolchain(self) -> None:
        """
        ユーザーが指定したエンジン (config の engine か LEANI_ENGINE) のバージョンを
        確認する。

        leani が用意したエンジンは使うバージョンでビルドしてあるので、バージョンは
        必ず一致する。ユーザーがビルドしたエンジンだけ、バージョンが違っていたら
        報告する (leani がビルドし直すことはしない)。
        """
        if self.cfg.engine is None:
            return

        engine_tc = (read_text(f"{self.dir}/lean-toolchain") or "").strip()
        if engine_tc and engine_tc != self.tc:
            print(
                yellow(
                    f"警告: toolchain が違う "
                    f"(使うバージョン={self.tc} / エンジン={engine_tc})。\n"
                    f"  cd {self.dir} && lake build repl"
                ),
                file=sys.stderr,
            )

    def _proc_env(self) -> dict[str, str]:
        """repl に渡す環境変数。エンジンの .olean の場所を LEAN_PATH の先頭に足す。"""
        env = dict(os.environ, **lake_env(self.cfg.project, self.tc))
        env["LEAN_PATH"] = f"{self.dir}/.lake/build/lib/lean:" + env.get(
            "LEAN_PATH", ""
        )
        return env

    # -- プロセス ---------------------------------------------------------

    def spawn(self) -> None:
        # 再起動するたびに世代を進める。前のプロセスの proofState と区別するため。
        self.gen += 1
        self.proc = subprocess.Popen(
            ["elan", "run", self.tc, f"{self.dir}/.lake/build/bin/repl"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=None,
            env=self.proc_env,
            text=True,
            encoding="utf-8",
            errors="replace",
            bufsize=1,
            start_new_session=True,  # Ctrl-C が repl に届かないようにする
        )

    def kill(self) -> None:
        if self.proc and self.proc.poll() is None:
            try:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                self.proc.kill()

    # -- プロトコル -------------------------------------------------------

    def send(self, obj: Json) -> Response:
        """
        リクエストを 1 つ送って、レスポンスを 1 つ読む。

        読み取りは fd を直接 select で待って行う。バッファ付きの readline では、
        Ctrl-C がどの時点で発生したのか (リクエストを送ったあとなのか、レスポンスを
        読み損ねたのか) が分からず、リクエストとレスポンスの対応がずれる恐れがある。
        ここで Interrupted を raise したら、呼び出し側は必ずエンジンを再起動する。
        """
        try:
            return self._exchange(obj)
        except KeyboardInterrupt:
            raise Interrupted() from None

    def _died(self) -> EngineDied:
        """
        プロセスがどう終了したかをエラーメッセージに含める。

        バージョンの合わない .olean や壊れたエンジンを読み込むと、repl は何も出力
        せずにシグナルで終了する。終了の仕方を記録しないと呼び出し側は理由を示せず、
        「import に失敗した」という推測だけが表示される。ユーザーは正しく書いた
        import を疑うところから調べることになる。

        EOF を読んだ直後は、まだ終了ステータスを取得できないことがあるので、少し待つ。
        """
        code = None
        if self.proc is not None:
            with contextlib.suppress(subprocess.TimeoutExpired):
                code = self.proc.wait(timeout=0.5)

        if code is None:
            return EngineDied("エンジンが応答しない")

        how = (
            f"異常終了した ({signal_name(-code)})"
            if code < 0
            else f"終了した (exit {code})"
        )
        return EngineDied(f"エンジンが{how}\n  {self.dir}/.lake/build/bin/repl")

    def _exchange(self, obj: Json) -> Response:
        if self.proc is None or self.proc.poll() is not None:
            raise self._died()

        # spawn は必ず PIPE で開くので None にはならないが、Popen の型からは
        # それが分からない。分かりにくい状態のまま進めず、異常終了と同じ扱いにする。
        stdin, stdout = self.proc.stdin, self.proc.stdout
        if stdin is None or stdout is None:
            raise self._died()

        try:
            stdin.write(json.dumps(obj) + "\n\n")
            stdin.flush()
        except (BrokenPipeError, ValueError) as e:
            raise self._died() from e

        fd = stdout.fileno()
        dec = codecs.getincrementaldecoder("utf-8")(errors="replace")
        buf = ""

        while True:
            try:
                ready, _, _ = select.select([fd], [], [], 0.25)
            except (OSError, ValueError) as e:
                raise self._died() from e

            if not ready:
                if self.proc.poll() is not None:
                    raise self._died()
                else:
                    continue

            try:
                chunk = os.read(fd, 1 << 16)
            except OSError as e:
                raise self._died() from e
            if not chunk:
                raise self._died()

            buf += dec.decode(chunk)
            done = first_response(buf)
            if done is not None:
                return done

    def send_cmd(self, src: str, fresh: bool = False) -> Response:
        """
        コマンドを 1 つ送る。`fresh` は「新しい環境を作る」ことを明示する引数。

        env があるかどうかは、このメソッドだけで確認する (probe_env は env を明示して
        send を直接呼ぶ)。
        `env` キーを付けずに送ると、repl はエラーにせず、Init だけの環境を新しく
        作ってその環境で答えてしまう。そのため「環境が無いときにどうするか」は
        必ず呼び出し側が決めることにして、決めていない呼び出しは送る前にエラーに
        する。boot が失敗した状態の扱いを書き忘れても、間違った結果を返すのでは
        なく `NoEnvironment` を raise する。
        """
        if fresh:
            return self.send({"cmd": src})
        elif self.env is None:
            raise NoEnvironment(
                f"環境が無いのにコマンドを送ろうとした: {head_line(src)}"
            )
        else:
            return self.send({"cmd": src, "env": self.env})

    def send_tactic(self, src: str, state: int) -> Response:
        return self.send({"tactic": src, "proofState": state})

    def query(self, src: str) -> str | None:
        """info メッセージの中身だけを取得する。環境は進めない。"""
        if self.env is None:
            # 環境が無いときは「答えられなかった」として None を返す。送れば
            # send_cmd がエラーにするが、型や補完の問い合わせは答えが無くても
            # 困らないので、例外にせず None を返して扱いを呼び出し側に任せる。
            return None

        resp = self.send_cmd(src)
        return None if has_error(resp) else info_text(resp)

    def advance(self, resp: Response) -> None:
        if self.env is not None:
            self.stack.append(self.env)
        self.env = resp["env"]

    def pop_decl(self) -> Undone:
        """直前の宣言を環境ごと取り消す。あとで元に戻せるよう、取り消した内容を返す。"""
        saved = Undone(self.env, self.log.pop() if self.log else None, self.gen)
        if self.stack:
            self.env = self.stack.pop()
        return saved

    def push_decl(self, saved: Undone) -> bool:
        """
        pop_decl で取り消したものを元に戻す。戻せなければ False を返す。

        世代が変わっていたら戻さない。保存した env id は終了したプロセスのもので、
        新しいエンジンには存在しない。それを今の env に設定すると、以後の cmd は
        存在しない環境に送られ (repl は "Unknown environment." を返すだけ)、
        何を入力しても何も起きなくなる。宣言は呼び出し側が再実行する。
        """
        if saved.gen != self.gen:
            return False

        if saved.src is not None:
            self.log.append(saved.src)
        if saved.env is not None and self.env is not None and saved.env != self.env:
            self.stack.append(self.env)
            self.env = saved.env

        return True

    def save_header(self) -> str:
        """
        :save が書き出すヘッダ。設定の import に、:l したファイルの import を追加する。

        追加しないと、:l したファイルが import していたモジュールがヘッダに
        含まれなくなる。その場合 :save は「宣言 n 件を書き出した」と成功を報告するのに、
        書き出したファイルは :l でも lean でもエラーになる (Unknown identifier が並ぶ)。
        """
        mods = ["Lean", *self.cfg.imports]
        mods += [m for m in self.loaded_imports if m not in mods]
        return "".join(f"import {m}\n" for m in mods)

    def sources(self) -> list[str]:
        """
        今の環境を構成しているソース。:save はこれを書き出す。

        log だけでは足りない。init と :l したファイルの内容は base の環境に含まれて
        いるので、それも並べないと、書き出したファイルを :l で読み込み直せない。

        並べる順は実際に実行した順にする。:l は環境を作り直すので、init はその
        あとに実行される (load_file が再適用する)。逆の順に並べると、init が :l
        したファイルの名前を使っているときに、書き出したファイルがエラーになる。
        """
        parts = [self.loaded_src, self.init_src, *self.log]
        return [src.strip() for src in parts if src and src.strip()]

    # -- 起動 / 再起動 ----------------------------------------------------

    def boot(self) -> None:
        """
        設定された import を実行して、最初の環境を作る。

        pickle によるキャッシュは試したが速くならないので採用していない。repl の
        pickle は import からの差分しか保存しない (1.2KB 程度) ので、unpickle でも
        .olean の読み込みに同じだけ時間がかかる。実測でも import 1.3s / unpickle
        1.2s、Mathlib は 5.4s / 5.3s で差が無い。さらに、unpickle した環境で #eval
        すると Lean のコンパイラが PANIC する。セッションの保存は :save (ソースの
        書き出し) で行う。
        """
        self.env, self.stack = None, []
        resp = self.send_cmd(self.cfg.boot_header + BOOT_PROBE, fresh=True)
        if has_error(resp):
            # ヘッダにエラーがあるか、import が 1 つでも解決できずにヘッダ全体が
            # 無視されたか。後者の場合 repl はエラーを出さずに env を返すので、
            # BOOT_PROBE が失敗することでしか検出できない。そのまま起動すると
            # import Lean も無い環境になり、完結判定も補完も宣言もすべて失敗する。
            # repl のメッセージもそのまま表示する。toolchain を変更して .olean の
            # バージョンが一致しなくなったときは、このメッセージにしか情報が無い
            # (import 自体は正しく書かれている)。
            why = error_text(resp)
            head = f"import に失敗した: {' '.join(['Lean', *self.cfg.imports])}"
            raise EngineDied(f"{head}\n{textwrap.indent(why, '  ')}" if why else head)

        self.env = self.base = resp["env"]

    def restart(self) -> Replay:
        """異常終了した / 中断されたエンジンを再起動し、宣言を replay する。"""
        self.kill()
        self.spawn()

        # boot が例外を raise しても、宣言は失わないようにする。やり直せば replay
        # できる。前回実行できなかった宣言は env に無いので、成功した宣言の後ろに
        # 並べる。
        log = list(self.log) + list(self.unplayed)
        loaded = self.loaded
        try:
            self.boot()
        except (EngineDied, Interrupted, OSError, KeyboardInterrupt):
            # boot が失敗した。プロセスは再起動したので前の env id は無効になって
            # いて、宣言はどの環境にも含まれていない。log に残すと len(stack) と
            # 一致しなくなり、:save が環境に無い宣言を本体に書き出す。保留にすれば
            # コメントとして書き出され、ユーザーは直してから :restart で再実行できる。
            # base も捨てる。:reset が無効な id を設定し直すと、submit の
            # 「env が無い」の確認で見つからなくなり、何を入力してもエラーになる。
            self.env = self.base = None
            self.stack, self.log, self.unplayed = [], [], log
            raise

        self.log, self.unplayed = [], []

        # init と :l したファイルの内容は base の環境に含まれていたので、再適用する。
        # ファイルを読み込み直せた場合は、load_file の中で init も再適用される。
        notes = []
        lost = self.reload(loaded)
        if lost:
            notes.append(lost)
        if lost or not loaded:
            note = self.reapply_init()
            if note:
                notes.append(note)

        out = self.replay(log)
        return out._replace(notes=notes + out.notes)

    def reload(self, loaded: str | None) -> str | None:
        """
        :l したファイルを読み込み直す。読み込み直せなかったときは理由を返す
        (None なら成功)。

        読み込み直せなかったのに loaded_src を残すと、sources() が env に無い宣言を
        返し続ける。そのまま :save すると、書き出したファイルを :l したときに
        「すでに宣言されている」というエラーになる。
        """
        if not loaded:
            return None

        try:
            out = self.load_file(loaded)
        except OSError as e:
            why = e.strerror or str(e)
        except (EngineDied, Interrupted):
            why = (
                "読み込み直している途中で、"
                "エンジンが異常終了したか、Ctrl-C で中断された"
            )
        else:
            if not out.bad:
                return out.note
            why = "読み込み直したらエラーになった"

        self.loaded, self.loaded_src = None, None
        return f"{loaded} を読み込み直せなかった: {why}"

    def reapply_init(self) -> str | None:
        """
        init を今の base の上でもう一度実行する。エラーになったら init_src を捨てて
        理由を返す。エンジンが異常終了したときや中断されたときは、init_src を残した
        まま理由を返す。

        init_src を捨てずに残すと、reload と同じく sources() が env に無い宣言を
        返すようになる。
        """
        if not self.init_src:
            return None

        try:
            resp = self.send_cmd(self.init_src)
        except (EngineDied, Interrupted):
            return (
                "init ファイルの再実行中に、"
                "エンジンが異常終了したか、Ctrl-C で中断された"
            )

        if has_error(resp):
            self.init_src = None
            return "init ファイルを再実行したらエラーになった"

        self.env = self.base = resp["env"]
        return None

    def probe_env(self, env: int) -> str | None:
        """
        その環境で import が有効になっているかを確認する。有効でなければ理由を返す。

        import が 1 つでも解決できないと、repl はヘッダ全体を無視して (エラーも
        出さずに) 環境を返す。boot と同じく :l でも BOOT_PROBE で検査しないと、
        「読み込んだ」と報告したあと、完結判定も補完も宣言もすべて失敗する環境で
        作業を続けることになる。:save も同じ import を書き出す。
        """
        resp = self.send({"cmd": BOOT_PROBE, "env": env})
        if not has_error(resp):
            return None

        why = error_text(resp)
        head = (
            "解決できない import があったので、"
            "repl はファイルの import をすべて無視した"
        )
        return f"{head}: {head_line(why)}" if why else head

    def replay(self, log: Sequence[str]) -> Replay:
        """
        宣言を今の環境でもう一度実行する。失敗したものは飛ばして続ける。

        :restart と、:env で環境を切り替えたあとに宣言を再実行する処理の両方が
        これを使う。失敗した宣言を何も表示せずに消さないよう、結果は Replay で返す。
        """
        done: list[str] = []
        failed: list[str] = []
        found: list[Sorry] = []

        if self.env is None:
            # 実行する環境が無い。送れば send_cmd がエラーにするが、これは
            # 「何件成功したか」を返す関数なので、例外を raise せずに全件を
            # 保留にし、理由を Replay に含めて返す。
            self.unplayed = list(log) + self.unplayed
            return Replay([], [], list(log), ["環境が無いので再実行できない"], [])

        for n, src in enumerate(log):
            try:
                resp = self.send_cmd(src)
            except (EngineDied, Interrupted):
                # まだ実行していない宣言は unplayed に保留する。エンジンを直せば
                # :restart でやり直せる。ここで捨てると、入力した宣言が失われる。
                #
                # log に加えてはいけない。log は env に含まれている宣言の並びで、
                # stack と 1 対 1 に対応している。env に無い宣言を加えると、
                # :undo と sorry の置き換えが別の宣言を消してしまう。
                rest = list(log[n:])
                self.unplayed = failed + rest + self.unplayed
                return Replay(done, failed, rest, [], found)

            if has_error(resp):
                # 宣言のテキストは保留にする。失敗の原因はユーザーの操作ではない
                # ので、「戻せなかった」と報告するだけで、入力された宣言を消しては
                # いけない。読み込むファイルを直せば、次の :restart で成功する。
                failed.append(src)
                continue

            self.advance(resp)
            self.log.append(src)
            done.append(src)
            # 最後に成功した宣言の sorry だけを保存する。証明中だった宣言は
            # log の末尾にあるので、これで :prove を再開できる。
            found = sorries(resp)

        self.unplayed = failed + self.unplayed
        return Replay(done, failed, [], [], found)

    def load_file(self, path: str, src: str | None = None) -> Loaded:
        if src is None:
            with open(path) as f:
                src = f.read()

        raw = src
        if not has_import(src):
            src = self.cfg.header + src
        src = PROBE_IMPORT + src

        keep = (self.env, self.stack, self.log, self.unplayed)
        self.env, self.stack, self.log, self.unplayed = None, [], [], []

        try:
            resp = self.send_cmd(src, fresh=True)
            why = None if has_error(resp) else self.probe_env(resp["env"])
        except (EngineDied, Interrupted, KeyboardInterrupt):
            # 中断や異常終了で、今の環境と宣言を失わないようにする。再起動すれば
            # replay できる。
            self.env, self.stack, self.log, self.unplayed = keep
            raise

        bad = has_error(resp) or why is not None
        note = why
        if not bad:
            self.env = self.base = resp["env"]
            self.loaded, self.loaded_src = path, strip_imports(raw)
            self.loaded_imports = import_lines(raw)
            # fresh で環境を作り直したので、init の内容は環境から消えている。
            # 再適用しないと、sources() が env に無い宣言を返し続ける。
            note = self.reapply_init()
        else:
            # 読み込みに失敗しても、今の環境は失わないようにする。env が None の
            # ままだと、以後の入力が import の無い環境に送られ、何を書いても
            # エラーになる。
            self.env, self.stack, self.log, self.unplayed = keep

        return Loaded(resp, bad, src, note)
