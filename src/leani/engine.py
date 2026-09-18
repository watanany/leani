"""エンジン (副作用)。

repl プロセス 1 個を抱え、JSON を往復させる。落ちたら作り直して replay する。"""

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
    """取り消した宣言 1 件。戻すのに要るものだけ。"""

    env: int | None
    src: str | None
    gen: int  # 控えたときのプロセスの世代。作り直されたら env id は死んでいる


class Loaded(NamedTuple):
    """load_file の結果。src は import を足したあとの、実際に送ったソース。"""

    resp: Response
    bad: bool
    src: str
    note: str | None = None  # init を重ね直せなかったときの理由


class Replay(NamedTuple):
    """
    replay の結果。落としたものを全部持って返す。

    件数だけ返していたら、通らなかった宣言も読み直せなかったファイルも黙って
    消えていた。環境から消えたものは必ず言う。
    """

    done: list[str]  # 通った宣言
    failed: list[str]  # 通らなかった宣言
    skipped: list[str]  # 打ち切って試していない宣言
    notes: list[str]  # init や :l で起きたこと
    sorries: list[Sorry]  # 最後に通った宣言に残った sorry

    @property
    def dropped(self) -> bool:
        """環境から消えたものがあるか。"""
        return bool(self.failed or self.skipped or self.notes)


class Engine:
    """repl サブプロセス 1 個。落ちたら restart() で作り直す。"""

    def __init__(self, cfg: EnvConfig) -> None:
        self.cfg = cfg
        self.tc = toolchain(cfg)
        self.dir = ensure_engine(cfg.engine, self.tc)
        self._warn_toolchain()
        self.proc_env = self._proc_env()
        self.proc: subprocess.Popen[str] | None = None
        self.env: int | None = None  # いまの環境 id
        self.base: int | None = None  # 起動直後 / :l 直後の環境 id
        self.stack: list[int] = []  # :undo 用
        self.log: list[str] = []  # 受理した宣言。再起動時に replay する
        self.unplayed: list[str] = []  # 打ったが env に入っていない宣言
        self.gen = 0  # プロセスの世代。proofState の持ち主の照合に使う
        self.loaded: str | None = None  # :l したファイル
        self.loaded_src: str | None = None  # その中身 (import は落とす)
        self.loaded_imports: list[str] = []  # そのファイルが書いていた import
        self.init_src: str | None = None  # init で通したソース
        self.spawn()

    # -- 環境変数 ---------------------------------------------------------

    def _warn_toolchain(self) -> None:
        """
        明示されたエンジンの版を確かめる。

        leani が用意したものは使う版でビルドしてあるので食い違わない。人が
        用意したものだけ、違っていたら言う (勝手に作り直さない)。
        """
        if self.cfg.engine is None:
            return

        engine_tc = (read_text(f"{self.dir}/lean-toolchain") or "").strip()
        if engine_tc and engine_tc != self.tc:
            print(
                yellow(
                    f"警告: toolchain が違う "
                    f"(使う版={self.tc} / エンジン={engine_tc})。\n"
                    f"  cd {self.dir} && lake build repl"
                ),
                file=sys.stderr,
            )

    def _proc_env(self) -> dict[str, str]:
        """repl に渡す環境変数。エンジンの olean を LEAN_PATH の先頭に足す。"""
        env = dict(os.environ, **lake_env(self.cfg.project, self.tc))
        env["LEAN_PATH"] = f"{self.dir}/.lake/build/lib/lean:" + env.get(
            "LEAN_PATH", ""
        )
        return env

    # -- プロセス ---------------------------------------------------------

    def spawn(self) -> None:
        # 作り直すたびに進める。前のプロセスの proofState と混ぜないため。
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
            start_new_session=True,  # Ctrl-C を自分のプロセス群だけに向ける
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
        リクエストを 1 つ投げて、レスポンス 1 つを読む。

        読み取りは生の fd と select でやる。バッファ付きの readline では
        Ctrl-C がどこで効いたのか (リクエストが飛んだのか、レスポンスを
        取りこぼしたのか) が分からず、プロトコルがずれる恐れがある。
        ここで Interrupted を上げたら呼び出し側は必ずエンジンを作り直す。
        """
        try:
            return self._exchange(obj)
        except KeyboardInterrupt:
            raise Interrupted() from None

    def _died(self) -> EngineDied:
        """
        死んだプロセスの終わり方を報告に載せる。

        版の合わない olean や壊れたエンジンを掴むと、repl は何も言わずに
        シグナルで消える。終わり方を残さないと呼び出し側は理由を言えず、
        「import が通らない」という当てずっぽうだけが残って、書き間違って
        いない import を疑うところから始めることになる。

        EOF を読んだ直後はまだ終了状態を拾えないことがあるので、少し待つ。
        """
        code = None
        if self.proc is not None:
            with contextlib.suppress(subprocess.TimeoutExpired):
                code = self.proc.wait(timeout=0.5)

        if code is None:
            return EngineDied("エンジンが応答しない")

        how = (
            f"落ちた ({signal_name(-code)})" if code < 0 else f"終了した (exit {code})"
        )
        return EngineDied(f"エンジンが{how}\n  {self.dir}/.lake/build/bin/repl")

    def _exchange(self, obj: Json) -> Response:
        if self.proc is None or self.proc.poll() is not None:
            raise self._died()

        # spawn は必ず PIPE で開くので None にはならないが、Popen の型は
        # それを知らない。落とし穴を残すより、死んだのと同じ扱いにする。
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
        コマンドを 1 つ送る。`fresh` は「新しい環境を作る」という意思表示。

        ここが唯一の関門。`env` キーを落として送ると repl はエラーにせず、
        Init だけの環境を勝手に作って答えてしまう。だから「環境が無いときに
        どうするか」は呼ぶ側が必ず決めることにして、決めていない呼び出しは
        送る前に断る。boot が通らなかった状態を扱い忘れても、嘘の答えでは
        なく `NoEnvironment` として出る。
        """
        if fresh:
            return self.send({"cmd": src})
        elif self.env is None:
            raise NoEnvironment(f"環境が無いのに送ろうとした: {head_line(src)}")
        else:
            return self.send({"cmd": src, "env": self.env})

    def send_tactic(self, src: str, state: int) -> Response:
        return self.send({"tactic": src, "proofState": state})

    def query(self, src: str) -> str | None:
        """info メッセージの中身だけ取る。環境は進めない。"""
        if self.env is None:
            # 環境が無いなら「答えられなかった」を返す。送れば send_cmd が
            # 関門で断るが、型や補完の問い合わせは答えが無くて済む種類の
            # ものなので、例外にせず None にして呼び手に任せる。
            return None

        resp = self.send_cmd(src)
        return None if has_error(resp) else info_text(resp)

    def advance(self, resp: Response) -> None:
        if self.env is not None:
            self.stack.append(self.env)
        self.env = resp["env"]

    def pop_decl(self) -> Undone:
        """直前の宣言を環境ごと取り消す。戻せるように控えを返す。"""
        saved = Undone(self.env, self.log.pop() if self.log else None, self.gen)
        if self.stack:
            self.env = self.stack.pop()
        return saved

    def push_decl(self, saved: Undone) -> bool:
        """
        pop_decl で取り消したものを戻す。戻せなければ False。

        世代が変わっていたら戻さない。控えた env id は死んだプロセスのもので、
        新しいエンジンには無い。それを今の env に据えると、以後の cmd は
        存在しない環境に飛び (repl は "Unknown environment." を返すだけ)、
        打っても何も起きない端末になる。宣言は呼ぶ側が流し直す。
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
        :save が書くヘッダ。設定の import に :l したファイルの import を足す。

        足さないと、:l したファイルが import していたものが落ちる。書き出しは
        「宣言 n 件を書き出した」と成功を報告するのに、そのファイルは :l でも
        lean でも通らない (Unknown identifier が並ぶ) という形で出る。
        """
        mods = ["Lean", *self.cfg.imports]
        mods += [m for m in self.loaded_imports if m not in mods]
        return "".join(f"import {m}\n" for m in mods)

    def sources(self) -> list[str]:
        """
        今の環境を作っているソース。:save がこれを書き出す。

        log だけでは足りない。init と :l したファイルは base に畳み込んで
        あるので、それも並べないと書き出したものを :l で読み直せない。

        並べる順は実際に流した順。:l は環境を作り直すので、init はその上に
        重なる (load_file が重ね直す)。逆に並べると、init が :l したファイルの
        名前を使っているときだけ書き出したファイルが通らなくなる。
        """
        parts = [self.loaded_src, self.init_src, *self.log]
        return [src.strip() for src in parts if src and src.strip()]

    # -- 起動 / 再起動 ----------------------------------------------------

    def boot(self) -> None:
        """
        設定された import を流して起点の環境を作る。

        pickle キャッシュは試したが効かないので入れていない。repl の pickle は
        import からの差分しか持たない (1.2KB 程度) ので、unpickle でも
        olean の読み込みは同じだけ走る。実測でも import 1.3s / unpickle 1.2s、
        mathlib は 5.4s / 5.3s で差が無い。さらに戻した環境で #eval すると
        Lean のコンパイラが PANIC する。セッションの保存は :save (ソース) で行う。
        """
        self.env, self.stack = None, []
        resp = self.send_cmd(self.cfg.boot_header + BOOT_PROBE, fresh=True)
        if has_error(resp):
            # ヘッダにエラーがあるか、import が 1 つでも解決できずに丸ごと
            # 捨てられたか。後者は repl が黙って env を返すので、BOOT_PROBE が
            # 通らないことでしか気付けない。そのまま起動すると import Lean も
            # 無い環境になり、完結判定も補完も宣言も全部通らなくなる。
            # 何と言われたかも出す。toolchain を差し替えて olean が食い違った
            # ときはここにしか手掛かりが無い (import 自体は書き間違っていない)。
            why = error_text(resp)
            head = f"import が通らない: {' '.join(['Lean', *self.cfg.imports])}"
            raise EngineDied(f"{head}\n{textwrap.indent(why, '  ')}" if why else head)

        self.env = self.base = resp["env"]

    def restart(self) -> Replay:
        """落ちた / 中断されたエンジンを作り直し、宣言を replay する。"""
        self.kill()
        self.spawn()

        # boot で投げたら log はそのまま残す。やり直せば replay できる。
        # 前回流せなかったぶんは env に無いので、通ったものの後ろに回す。
        log = list(self.log) + list(self.unplayed)
        loaded = self.loaded
        try:
            self.boot()
        except (EngineDied, Interrupted, OSError, KeyboardInterrupt):
            # boot が通らなかった。プロセスは作り直したので前の env id は死んで
            # いて、宣言はどこにも入っていない。log に残すと len(stack) と
            # 食い違い、:save が環境に無い宣言を本体に書く。控えに回せば
            # コメントとして添えられ、直してから :restart で流し直せる。
            # base も捨てる。:reset が死んだ id を据え直すと、submit の
            # 「env が無い」ガードが外れて何を打っても通らない端末になる。
            self.env = self.base = None
            self.stack, self.log, self.unplayed = [], [], log
            raise

        self.log, self.unplayed = [], []

        # init と :l したファイルは base に畳み込んであるので重ね直す。
        # ファイルを読み直せた場合は load_file の中で init も重なる。
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
        :l したファイルを読み直す。読めなかった理由を返す (None なら成功)。

        読み直せなかったのに loaded_src を残すと、env に無い宣言を sources()
        が並べ続ける。そのまま :save すると、書き出したファイルが :l で
        「すでに宣言されている」と言って通らない。
        """
        if not loaded:
            return None

        try:
            out = self.load_file(loaded)
        except OSError as e:
            why = e.strerror or str(e)
        except (EngineDied, Interrupted):
            why = "読み直している途中で止まった"
        else:
            if not out.bad:
                return out.note
            why = "読み直したら通らなかった"

        self.loaded, self.loaded_src = None, None
        return f"{loaded} を読み直せなかった: {why}"

    def reapply_init(self) -> str | None:
        """
        init を今の base に重ね直す。重ねられなければ理由を返して忘れる。

        忘れずに init_src を残すと、これも sources() が env に無い宣言を
        並べる側に回る。
        """
        if not self.init_src:
            return None

        try:
            resp = self.send_cmd(self.init_src)
        except (EngineDied, Interrupted):
            return "init を重ね直している途中で止まった"

        if has_error(resp):
            self.init_src = None
            return "init を重ね直せなかった"

        self.env = self.base = resp["env"]
        return None

    def probe_env(self, env: int) -> str | None:
        """
        その環境で import が効いているかを確かめる。効いていなければ理由を返す。

        import が 1 つでも解決できないと、repl はヘッダを丸ごと捨てて (エラーも
        出さずに) 環境を返す。boot はそれを BOOT_PROBE で見ているが、:l には
        同じ確かめが無かったので「読み込んだ」と報告してから、完結判定も補完も
        宣言も全部通らない環境に座ることになる。:save も同じ import を書く。
        """
        resp = self.send({"cmd": BOOT_PROBE, "env": env})
        if not has_error(resp):
            return None

        why = error_text(resp)
        head = "import が解決できないのでヘッダが丸ごと捨てられた"
        return f"{head}: {head_line(why)}" if why else head

    def replay(self, log: Sequence[str]) -> Replay:
        """
        宣言を今の環境に流し直す。通らなかったものは飛ばして続ける。

        :restart と :env の戻り道が同じものを使う。件数だけ返していたころは
        通らなかった宣言が黙って消えていた。
        """
        done: list[str] = []
        failed: list[str] = []
        found: list[Sorry] = []

        if self.env is None:
            # 流し直す先が無い。送れば send_cmd が関門で断つが、ここは
            # 「何件通ったか」を返す関数なので、例外を上げずに全件を控えへ
            # 回し、理由を Replay に載せて返す。
            self.unplayed = list(log) + self.unplayed
            return Replay([], [], list(log), ["環境が無いので流し直せない"], [])

        for n, src in enumerate(log):
            try:
                resp = self.send_cmd(src)
            except (EngineDied, Interrupted):
                # 試せていないものは unplayed に控える。エンジンを直せば
                # :restart でやり直せる。ここで捨てると打ったものが戻らない。
                #
                # log に混ぜてはいけない。log は env に入っている宣言の並びで、
                # stack と 1 対 1 に対応している。env に無いものを混ぜると
                # :undo と埋め戻しが別の宣言を落とす。
                rest = list(log[n:])
                self.unplayed = failed + rest + self.unplayed
                return Replay(done, failed, rest, [], found)

            if has_error(resp):
                # テキストは控えに回す。落ちたのはユーザの操作ではないので、
                # 「戻せなかった」と言うだけで打ったものを消してはいけない。
                # 読み込むファイルを直せば次の :restart で通る。
                failed.append(src)
                continue

            self.advance(resp)
            self.log.append(src)
            done.append(src)
            # 最後に通ったものだけ覚える。証明していた宣言は log の末尾に
            # 居るので、これで :prove に繋ぎ直せる。
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
            # 中断や落ちで手元の環境と宣言を失わない。作り直せば replay できる。
            self.env, self.stack, self.log, self.unplayed = keep
            raise

        bad = has_error(resp) or why is not None
        note = why
        if not bad:
            self.env = self.base = resp["env"]
            self.loaded, self.loaded_src = path, strip_imports(raw)
            self.loaded_imports = import_lines(raw)
            # fresh で環境を作り直したので init は落ちている。重ね直さないと
            # env には無いものを sources() が並べ続ける。
            note = self.reapply_init()
        else:
            # 失敗した読み込みで手元の環境まで失わない。env が None のままだと
            # 以後の入力が import 無しの環境に飛んで、何を書いても通らなくなる。
            self.env, self.stack, self.log, self.unplayed = keep

        return Loaded(resp, bad, src, note)
