"""フロント (副作用)。

入力を読み、送り方を決め、結果を出す。入力バッファと証明モードを持つ。"""

from __future__ import annotations

import contextlib
import json
import os
import subprocess
import sys
import textwrap
import time
from collections.abc import Callable, Iterator, Sequence
from dataclasses import dataclass, field

from prompt_toolkit import PromptSession
from prompt_toolkit.completion import CompleteEvent, Completer, Completion
from prompt_toolkit.document import Document
from prompt_toolkit.formatted_text import ANSI
from prompt_toolkit.history import FileHistory
from prompt_toolkit.key_binding import KeyBindings
from prompt_toolkit.key_binding.key_processor import KeyPressEvent
from prompt_toolkit.shortcuts import CompleteStyle

from leani.abbrev import expand_abbrev
from leani.boot import prepare
from leani.config import EnvConfig, load_config, problem, resolve
from leani.engine import Engine, Replay, Undone
from leani.places import COMPLETE_CAP, HIST, INIT, TTY
from leani.pure import (
    balanced,
    block_continues,
    c,
    classify,
    classify_tac,
    continues,
    dim,
    errors,
    green,
    has_error,
    head_line,
    lean_str,
    messages,
    red,
    sorries,
    splice_sorry,
    strip_imports,
    try_this,
    yellow,
)
from leani.queries import (
    COMPLETE_QUERY,
    DECL_NAME,
    DOC_QUERY,
    META_LINE,
    NOT_EVALUABLE,
    PARSE_PROBE,
)
from leani.show import die, panic_check, render
from leani.types import (
    CMD,
    MORE,
    START_FAILED,
    TERM,
    ConfigError,
    EngineDied,
    EngineError,
    Interrupted,
    Json,
    Kind,
    Sorry,
    State,
    Step,
    T,
)


def abbrev_keys() -> KeyBindings:
    """
    space に略記の確定を割り当てる。

    Tab は補完が使っているので触らない。ここで Tab も兼ねると、同じ打鍵が
    手前の文字次第で補完にも変換にもなって、どちらが起きるか打つ前に読めない。

    space はそのまま入れる。確定の合図を食べてしまうと `a \\to b` が `a →b` に
    なって、記号を出すたびに space を打ち足すことになる。表に無ければ何も
    起きないので、space が space でなくなる場面は作らない。
    """
    kb = KeyBindings()

    @kb.add(" ")
    def _(event: KeyPressEvent) -> None:
        buf = event.current_buffer
        got = expand_abbrev(buf.document.text_before_cursor)
        if got is not None:
            sym, back = got
            buf.delete_before_cursor(back)
            buf.insert_text(sym)

        buf.insert_text(" ")

    return kb


HELP = """\
式を書くと #eval される。宣言はそのまま通る。入力が終わったかは Lean のパーサが決める。
続きがある行はそのまま次の行を待ち、インデントを続ける限り読む。空行で確定。
確定した直後にインデント行を書けば、直前の入力に遡って続きとして読み直す。

  :t <expr>      型 (#check)
  :i <name>      型と docstring
  :p <name>      定義 (#print)
  :l <file>      読み込む (環境を作り直す)   :r  読み直す
  :reset         起動直後に戻る              :undo [n]  n 個前の環境へ
  :env [name]    今の環境 / 設定した環境に切り替えて再起動
  :prove [n]     sorry の証明モードに入る   :goals  残っている sorry と目標
  :save <file>   通した宣言を .lean に書き出す (:l で読み直せる)
  :time          実行時間の表示を切り替え
  :{ ... :}      複数行を明示的に囲む
  :! <cmd>       shell
  :restart       エンジンを作り直して宣言を replay
  :help :?       これ                        :q  終了 (Ctrl-D)

証明モード (⊢>) では 1 行が 1 タクティク。:goals :script :undo :done
"""

PROOF_HELP = (
    "証明モード: 1 行 = 1 タクティク。:goals 目標  :script 台本  :undo 戻す  :done 出る"
)


@dataclass
class Proof:
    """証明モードの状態。1 行 = 1 タクティクで進む。"""

    state: int  # repl 側の proofState
    goals: list[str]
    script: list[str] = field(default_factory=list)
    stack: list[int] = field(default_factory=list)  # :undo 用


@dataclass(frozen=True)
class Last:
    """直前に送った入力。インデント行が来たときに遡るために覚えておく。"""

    src: str
    advanced: bool  # 環境を進めたか (取り消すべきか)
    proof: bool = False  # 証明モードでのタクティクだったか
    env_before: int | None = None


COMPLETE_DELIMS = ' \t\n(),[]{};"'


class BlockHistory(FileHistory):
    """
    確定した入力を 1 件として持つ履歴。

    prompt_toolkit は prompt() を抜けるたびにその 1 行を入れようとするが、
    `def fib` の 4 行が 4 件になると呼び戻すのに Ctrl-P が 4 回要る。何をもって
    1 件とするかは leani 側が知っている (submit / discard) ので、勝手な追加は
    捨てて record だけを受ける。

    ファイルへは追記しかしない。読めない形式のファイル (readline や libedit の
    履歴) があっても、行が無視されるだけで書き潰さない。
    """

    def append_string(self, string: str) -> None:
        pass

    def record(self, string: str) -> None:
        super().append_string(string)


class NameCompleter(Completer):
    """Tab で定数名を補う。候補は Repl が出す。"""

    def __init__(self, names: Callable[[str], list[str]]) -> None:
        self.names = names

    def get_completions(
        self, document: Document, complete_event: CompleteEvent
    ) -> Iterator[Completion]:
        # Lean の名前は . を含むので区切りにしない。
        text = document.text_before_cursor
        head = max(text.rfind(d) for d in COMPLETE_DELIMS)
        prefix = text[head + 1 :]
        for name in self.names(prefix):
            yield Completion(name, start_position=-len(prefix))


class Repl:
    """
    端末との対話。副作用の層。

    入力を 1 行受けて (feed)、完結したかをパーサに聞き (probe)、送って (submit)
    表示する (render) という流れで読める。判定と整形は上の純粋な関数に出して
    あるので、ここに残るのは状態遷移と入出力だけ。持っている状態は入力バッファ
    (buf / ready / explicit)、証明モード (proof / pending)、補完のキャッシュ。
    """

    # 行編集は 1 セッションを使い回す。:env で Repl を作り直しても履歴は続く。
    _session: PromptSession[str] | None = None
    _history: BlockHistory | None = None

    def __init__(self, cfg: EnvConfig, preload: str | None = None) -> None:
        self.saved: set[str] = set()  # :save で書いたもの。上書きの判断に使う
        self._start(cfg, preload)

    def _start(self, cfg: EnvConfig, preload: str | None = None) -> None:
        """エンジンを立てて起点の環境を作る。:env の切り替えでもここを通る。"""
        self.cfg = cfg

        # 入力バッファ
        self.buf: list[str] = []
        self.ready: Kind | None = None  # 構文的に完結しているときの送り方
        self.last: Last | None = None
        self.explicit = False  # :{ ... :} の中か
        self.undone: Undone | None = None  # rewind で取り消した宣言
        self.show_time = False

        # 証明モード
        self.proof: Proof | None = None
        self.proof_gen = -1  # その proof を出したエンジンの世代
        self.pending: list[Sorry] = []
        self.proof_at: Sorry | None = None  # :prove で選んだ sorry (位置つき)
        self.proof_src: str | None = None  # sorry を出した宣言のソース
        self.sorry_env: int | None = None  # その宣言が作った環境 id (照合用)

        # 補完と履歴
        self._comp_cache: dict[tuple[int | None, str], list[str]] = {}
        self._own: tuple[int, list[str]] = (-1, [])

        self.eng = Engine(cfg)
        try:
            self._setup_prompt()
            t0 = time.time()
            self.eng.boot()
        except BaseException:
            # Engine を作った時点で repl は起動している。ここで投げると
            # 呼び側が self.eng を差し替えるので、殺す手立てが無くなる
            # (:env の切り替えに失敗するたび 1 プロセス残っていた)。
            self.eng.kill()
            raise
        print(dim(f"leani — {self.eng.tc} / {cfg} / {time.time() - t0:.1f}s"))

        if os.path.isfile(INIT):
            self.apply_init()
        if preload:
            self.load(preload)

    def apply_init(self) -> None:
        """
        init ファイルを起動直後の環境に重ねる。

        :l と違って環境を作り直さない。ここで通したものは base に含めるので
        :reset しても残る (GHCi の .ghci と同じ扱い)。
        """
        try:
            with open(INIT) as f:
                text = f.read()
        except OSError as e:
            print(red(f"{INIT} が読めない ({e.strerror})"))
            return

        src = strip_imports(text)
        if not src.strip():
            return

        resp = self.guard(lambda: self.eng.send_cmd(src))
        if resp is None:
            return
        elif has_error(resp):
            print(red(f"{INIT} にエラーがある:"))
            render(resp, src)
            return
        else:
            # 作り直したときに重ね直せるよう Engine に持たせる。
            self.eng.env = self.eng.base = resp["env"]
            self.eng.init_src = src
            print(dim(f"-- {INIT} を読んだ"))

    # -- 行編集 -----------------------------------------------------------

    def _setup_prompt(self) -> None:
        """
        prompt_toolkit のセッションを用意する。端末でなければ持たない。

        補完は Repl に紐付くので、:env で作り直したらそのつど差し替える。
        セッションと履歴そのものは使い回して、切り替えても Ctrl-P が続く
        ようにする。
        """
        if not sys.stdin.isatty() or not TTY:
            return

        if Repl._history is None:
            if os.path.dirname(HIST):
                with contextlib.suppress(OSError):
                    os.makedirs(os.path.dirname(HIST), exist_ok=True)
            Repl._history = BlockHistory(HIST)

        if Repl._session is None:
            Repl._session = PromptSession(
                history=Repl._history,
                key_bindings=abbrev_keys(),
                # Tab を押したときだけ聞く。打つたびに聞くと mathlib では
                # 1 打鍵ごとにエンジンへ問い合わせることになる。
                complete_while_typing=False,
                # 共通部分まで補いつつ候補を下に並べる。READLINE_LIKE は
                # 候補の一覧を in_terminal (CPR の往復を待つ) で出すので、
                # 端末が答えるまで何も出ない。
                complete_style=CompleteStyle.MULTI_COLUMN,
            )

        Repl._session.completer = NameCompleter(self._names)

    def remember(self, src: str) -> None:
        """履歴に 1 件として入れる。複数行の宣言もこれで丸ごと 1 件になる。"""
        if Repl._history is not None and src.strip():
            Repl._history.record(src.rstrip())

    # -- 補完 -------------------------------------------------------------

    @staticmethod
    def _chunk(prefix: str) -> str:
        """
        まとめて取る単位。名前空間があればそこまで、無ければ先頭 2 文字。

        mathlib では 1 文字だと `C` で 7.5 万件 (3.4MB) になるので広げすぎない。
        `Nat.` なら 5684 件、`MeasureTheory.` なら 1 万件で収まる。
        """
        return prefix[: prefix.rfind(".") + 1] if "." in prefix else prefix[:2]

    def _names(self, prefix: str) -> list[str]:
        """定数名の prefix 検索。名前空間ごとの塊を 1 度だけ取って以後は絞る。"""
        if len(prefix) < 2:
            return []

        # 塊は base 環境 (import / :l 直後) に紐付ける。宣言を 1 つ通すたびに
        # 捨てていると mathlib では毎回 1.1 秒かかり直すので、自分で通した分だけ
        # Python 側で足す。
        key = (self.eng.base, self._chunk(prefix))
        chunk = self._comp_cache.get(key)
        if chunk is None:
            out = self.guard(
                lambda: self.eng.query(
                    COMPLETE_QUERY % (lean_str(key[1]), COMPLETE_CAP)
                )
            )
            if out is None:
                return []

            chunk = out.split()
            self._comp_cache[key] = chunk

        hits = {x for x in chunk if x.startswith(prefix)}
        hits.update(x for x in self._own_names() if x.startswith(prefix))
        return sorted(hits)

    def _own_names(self) -> list[str]:
        """REPL で通した宣言の名前。ログが伸びたときだけ数え直す。"""
        if self._own[0] != len(self.eng.log):
            names: set[str] = set()
            for src in self.eng.log:
                names.update(DECL_NAME.findall(src))
            self._own = (len(self.eng.log), sorted(names))

        return self._own[1]

    # -- エンジンの面倒を見る ---------------------------------------------

    def revive(self) -> Replay | None:
        """
        エンジンを作り直して replay する。作り直せなければ None。

        boot が通らない (import が壊れている / Ctrl-C で中断した) ことは
        ふつうに起きる。宣言は Engine 側に残るので、直してから :restart で
        やり直せる。ここで投げるとセッションごと消えるので投げない。
        """
        try:
            out = self.eng.restart()
        except Interrupted:
            print(yellow("^C 作り直しを中断した。:restart でやり直せる"))
            self.fold_proof()
            return None
        except (EngineDied, OSError) as e:
            print(red(f"エンジンを作り直せなかった: {str(e) or '理由は分からない'}"))
            print(dim("  原因を直してから :restart"))
            self.fold_proof()
            return None

        self.proof, self.last = None, None
        self.clear_pending()
        print(dim(f"宣言 {len(out.done)} 件を replay した (env {self.eng.env})"))
        self.report_replay(out)
        self.reattach(out)
        return out

    def fold_proof(self) -> None:
        """
        作り直しに失敗したときの後始末。環境が無いので遡る先も無い。

        成功パスだけが証明モードを畳んでいたので、失敗すると死んだ
        proofState を掴んだままになり、以降どの行も赤い "Unknown proof state."
        だけを返す幽霊の証明モードに座り続けていた (抜ける案内も出ない)。
        """
        self.last = None
        if self.proof is not None:
            self.drop_proof()
        # 持ち越した proofState も死んだプロセスのもの。残すと :goals が
        # 環境に無い宣言の目標を出し、:prove がその幽霊で証明モードに入る。
        self.clear_pending()

    def report_replay(self, out: Replay) -> None:
        """replay で落としたものを言う。黙って消えると気付く場所が無い。"""
        for note in out.notes:
            print(yellow(note))
        for src in out.failed:
            print(yellow(f"戻せなかった宣言: {head_line(src)}"))
        if out.failed or out.notes:
            print(dim("  これらは環境に無い (テキストは控えてある)"))
        if out.skipped:
            print(yellow(f"まだ流していない宣言: {len(out.skipped)} 件"))
            print(dim("  これらも環境に無い。:restart でやり直せる"))

    def reattach(self, out: Replay) -> None:
        """
        replay で戻った宣言に sorry が残っていたら :prove に繋ぎ直す。

        タクティクの途中で落ちた / Ctrl-C したとき、証明していた宣言は
        replay で戻っている。新しい proofState を拾い直さないと、宣言はある
        のに :prove が「sorry が無い」と言うだけになり、:undo しか道が無い。
        """
        if not out.sorries or not out.done:
            return

        self.pending = out.sorries
        self.proof_src = out.done[-1]
        self.sorry_env = self.eng.env
        print(dim(f"-- :prove で証明モードに入り直せる (sorry {len(out.sorries)} 個)"))

    def replay_into(self, log: Sequence[str]) -> None:
        """打った宣言を今のエンジンに流し直す。:env の戻り道で使う。"""
        if not log:
            return

        if self.eng.env is None:
            # ここは案内を出す層。関門の例外をそのまま「内部エラー」として
            # 見せると、控えが残っていることも次の一手も伝わらない。
            print(yellow(f"環境が無いので宣言 {len(log)} 件を戻せなかった"))
            print(dim("  テキストは控えてある。:restart で建て直せる"))
            self.eng.unplayed = list(log) + self.eng.unplayed
            return

        out = self.guard(lambda: self.eng.replay(log))
        if out is None:
            print(yellow(f"打った宣言 {len(log)} 件を戻せなかった"))
            return

        print(dim(f"宣言 {len(out.done)} 件を戻した (env {self.eng.env})"))
        self.report_replay(out)
        self.reattach(out)

    def guard(self, fn: Callable[[], T], on_dead: T | None = None) -> T | None:
        """
        エンジンが落ちる / 中断されたら再起動して replay する。

        落ちた場合は作り直したうえで 1 回だけやり直す。ユーザが Ctrl-C で
        止めた場合はやり直さない (止めたいのだから)。
        """
        retry = False
        try:
            return fn()
        except Interrupted:
            print(yellow("^C 中断した。エンジンを作り直す…"))
        except EngineDied:
            print(red("エンジンが落ちた。作り直す…"))
            retry = True

        if self.revive() is None:
            return on_dead

        if retry:
            try:
                return fn()
            except (Interrupted, EngineDied):
                pass

        return on_dead

    # -- 完結判定 ---------------------------------------------------------

    def parse(self, src: str) -> Json | None:
        """
        Lean のパーサに command / term / tacticSeq として読めるかを聞く。
        1 往復で 3 つとも取る。実行はしないので、ユーザ定義の notation も効く。
        """
        out = self.guard(lambda: self.eng.query(PARSE_PROBE % lean_str(src)))
        if not out:
            return None

        try:
            got = json.loads(out.strip().splitlines()[-1])
        except (json.JSONDecodeError, IndexError):
            return None

        return got if isinstance(got, dict) else None

    def probe(self, src: str) -> tuple[State, Kind]:
        return classify(self.parse(src))

    def probe_tac(self, src: str) -> tuple[State, Kind]:
        return classify_tac(self.parse(src))

    # -- ループ -----------------------------------------------------------

    def prompt(self) -> str:
        if self.proof is not None:
            return c("35", "⊢> ")
        else:
            return c("36", self.cfg.prompt)

    def read_line(self, prompt: str) -> str:
        """
        1 行読む。Ctrl-C は KeyboardInterrupt、Ctrl-D は EOFError で上に返る。

        端末を握るのは prompt() の中だけなので、評価中の Ctrl-C は今までどおり
        SIGINT として届く (Engine.send が Interrupted に訳す)。端末でなければ
        セッションを持たないので、パイプ入力は素の input() を通る。
        """
        if Repl._session is None:
            return input(prompt)
        else:
            return Repl._session.prompt(ANSI(prompt))

    def loop(self) -> int:
        while True:
            try:
                line = self.read_line(
                    c("2", " | ") if (self.buf or self.explicit) else self.prompt()
                )
            except EOFError:
                if self.buf or self.explicit:
                    self.discard()
                    print()
                    continue
                else:
                    print()
                    return 0
            except KeyboardInterrupt:
                self.discard()
                self.last = None
                print("^C")
                continue
            except UnicodeDecodeError as e:
                # tty でない stdin (パイプ) は strict デコードになる。ここで
                # 抜けると、それまでに通した宣言ごと落ちる。その行だけ捨てる。
                print(red(f"UTF-8 として読めない行を飛ばした ({e.reason})"))
                continue

            try:
                if self.feed_line(line) == "quit":
                    return 0
            except KeyboardInterrupt:
                # Engine.send の中は Interrupted に翻訳されるが、その外
                # (子プロセス・整形・補完) で来たぶんはここに落ちる。
                self.discard()
                self.last = None
                print("^C")
            except Exception as e:
                # 想定外でも 1 行分のエラーで済ませる。セッションを畳むと
                # そこまでの宣言を全部失うので、それが一番高い代償になる。
                print(red(f"内部エラー: {type(e).__name__}: {e}"))
                self.discard()

    def feed_line(self, line: str) -> Step | None:
        """端末から来た 1 件。履歴から戻ったものは改行入りで来る。"""
        # まとめた履歴を呼び戻すと改行入りの 1 行として返ってくるので、
        # 打ったときと同じ順に食わせ直す。
        lines = line.split("\n")
        for one in lines:
            if self.feed(one) == "quit":
                return "quit"

        # 呼び戻した複数行はまとめて 1 件なので、末尾に空行を足して確定させる。
        waiting = bool(self.buf or self.explicit)
        if len(lines) > 1 and waiting:
            return self.feed("")
        else:
            return None

    def discard(self) -> None:
        """入力中のブロックを捨てる。Ctrl-C / Ctrl-D で呼ぶ。"""
        # 打ったものは履歴に残す。捨てたのは入力バッファであって、打鍵の記録
        # ではない。長い宣言を打ち間違えたときに Ctrl-P で取り戻せる。
        self.remember("\n".join(self.buf))
        self.buf, self.ready, self.explicit = [], None, False
        self.restore_undone()

    def feed(self, line: str) -> Step | None:
        """
        1 行受け取る。"quit" を返したらループを抜ける。

        完結したかはパーサに聞くが、それだけでは足りない。Lean では
        `structure P where` や `def f := 1` はそれ自体で完結した command なので、
        パーサは「終わり」と言う。にもかかわらず次のインデント行は続きになりうる。
        そこで二段構えにする:

        * パーサが「途中」と言えば継続行を読む (ブロックに入る)。
        * ブロック中は、インデント行が続く限り読む。空行かインデントの切れた行で確定。
        * ブロックに入らず確定したあとにインデント行が来たら、直前の入力に
          遡って続きとして読み直す (環境も 1 つ戻す)。
        """
        if self.explicit:
            step = self.feed_explicit(line)
        elif not self.buf:
            step = self.feed_first(line)
        else:
            step = self.feed_more(line)

        if step != "probe":
            return "quit" if step == "quit" else None

        src = "\n".join(self.buf)
        proving = self.proof is not None
        state, kind = self.probe_tac(src) if proving else self.probe(src)
        if proving and self.proof is None:
            # プローブ自体が guard 経由でエンジンの死を踏み、revive が証明モードを
            # 畳んだ。このまま submit すると self.proof を見ないので command 経路に
            # 落ち、タクティクの行が宣言として送られて
            # "unexpected identifier; expected command" になる。しかも
            # submit_cmd の clear_pending が reattach の成果を消すので、直前に出した
            # 「:prove で入り直せる」まで嘘になる。行は捨てて案内だけ残す。
            self.remember("\n".join(self.buf))
            self.buf, self.ready = [], None
            print(dim("  打っていた行は送らなかった"))
            return None
        elif state == MORE:
            self.ready = None
            return None
        elif block_continues(self.buf, src):
            self.ready = kind  # 完結。ただし続きがありうるので空行を待つ
            return None
        else:
            self.buf, self.ready = [], None
            self.submit(src, kind)
            return None

    def feed_explicit(self, line: str) -> Step:
        """:{ ... :} の中。:} が来るまで何も判定せずに溜める。"""
        if line.strip() == ":}":
            src, self.buf, self.explicit = "\n".join(self.buf), [], False
            if src.strip():
                self.submit(src)
        else:
            self.buf.append(line)
        return "done"

    def feed_first(self, line: str) -> Step:
        """バッファが空のときの 1 行目。"""
        s = line.strip()
        if not s:
            self.last = None
            return "done"
        elif s == ":{":
            self.explicit, self.buf = True, []
            return "done"
        elif s.startswith(":"):
            self.last = None
            self.remember(s)
            return "quit" if self.meta(s) == "quit" else "done"
        elif continues(line) and self.last is not None:
            # 確定した入力の続きだった。1 つ戻して書き直す。
            self.rewind()
            self.buf = self.last.src.splitlines() + [line]
            self.last = None
            return "probe"
        else:
            self.buf = [line]
            return "probe"

    def feed_more(self, line: str) -> Step:
        """ブロックの 2 行目以降。"""
        if not line.strip():  # 空行で確定
            src, kind = "\n".join(self.buf).rstrip(), self.ready
            self.buf, self.ready = [], None
            if src.strip():
                self.submit(src, kind)
            return "done"
        elif META_LINE.match(line):  # ブロックからの脱出
            src, kind = "\n".join(self.buf), self.ready
            self.buf, self.ready = [], None
            if kind is not None:
                self.submit(src, kind)
            else:
                self.remember(src)
                self.restore_undone()
                print(dim("-- 未完のまま破棄した"))
                self.last = None
            return "quit" if self.feed(line) == "quit" else "done"
        elif self.ready is not None and not continues(line):
            # 確定済みのブロックにインデントの切れた行 → ここで切って読み直す
            src, kind = "\n".join(self.buf), self.ready
            self.buf, self.ready = [], None
            self.submit(src, kind)
            return "quit" if self.feed(line) == "quit" else "done"
        else:
            self.buf.append(line)
            return "probe"

    def rewind(self) -> None:
        """直前の入力を取り消して、続きを書けるようにする。"""
        last = self.last
        if last is None or not last.advanced:
            return
        elif last.proof:
            # 証明モードのタクティクは環境を進めていない。畳まれていても
            # ここで宣言を pop してはいけない (直前の本物の宣言が消える)。
            if self.proof is None:
                return
            if self.proof.stack:
                self.proof.state = self.proof.stack.pop()
            if self.proof.script:
                self.proof.script.pop()
            return
        else:
            # 書き直しをやめたときに戻せるよう控える (C-c / C-d / ブロック脱出)。
            self.undone = self.eng.pop_decl()

    def restore_undone(self) -> None:
        """rewind で取り消した宣言を戻す。書き直さずにやめたとき。"""
        if self.undone is None:
            return

        undone, self.undone = self.undone, None
        if not self.eng.push_decl(undone) and undone.src is not None:
            # 控えている間にエンジンが建て直された。控えた env id は死んで
            # いるので据えられない。テキストから流し直す。
            self.replay_into([undone.src])

    # -- 送信 -------------------------------------------------------------

    def submit(self, src: str, kind: Kind | None = None) -> None:
        """完結した入力を送って結果を出す。"""
        # 送れるかを見る前に履歴へ入れる。エンジンが死んでいるときこそ、
        # 打ったものを呼び戻せないと困る。
        self.remember(src)

        if self.eng.env is None:
            # boot が通らなかったエンジン。送れば send_cmd が関門で断るが、
            # 打った本人に要るのは例外の名前ではなく次の一手なので、ここで
            # 案内に変える。
            print(red("エンジンが使えない。:restart で建て直す"))
            return

        self.undone = None  # 書き直しが確定した。もう戻さない
        if self.proof is not None:
            self.tactic(src)
            return

        if kind is None:
            _, kind = self.probe(src)

        t0 = time.time()
        self.last = None
        sent = self.submit_term(src) if kind == TERM else self.submit_cmd(src)
        if sent:
            self.timing(t0)

    def submit_term(self, src: str) -> bool:
        """式として #eval に包んで送る。環境は進めない。"""
        wrapped = "#eval\n" + textwrap.indent(src, "  ")
        resp = self.guard(lambda: self.eng.send_cmd(wrapped))
        if resp is None or panic_check(resp):
            return False

        errs = errors(resp)
        blob = "\n".join(m.get("data", "") for m in errs)

        # `do` を単体で書くと最初の action からモナドが決まってしまう
        # (IO.getEnv なら BaseIO)。GHCi と同じく IO と読み直してやる。
        if errs and src.lstrip().startswith("do") and "BaseIO" in blob:
            retry = "#eval show IO _ from\n" + textwrap.indent(src, "  ")
            again = self.guard(lambda: self.eng.send_cmd(retry))
            if again is not None and not has_error(again):
                render(again, src, line_off=1, col_off=2)
                self.last = Last(src, advanced=False)
                return True

        # 評価できない式でも、型だけは出したほうが親切。
        if errs and NOT_EVALUABLE.search(blob):
            out = self.guard(
                lambda: self.eng.query("#check\n" + textwrap.indent(src, "  "))
            )
            if out:
                print(out.rstrip())
                print(dim("-- 評価できないので型だけ"))
                self.last = Last(src, advanced=False)
                return True

        render(resp, src, line_off=1, col_off=2)
        self.last = Last(src, advanced=False)
        return True

    def submit_cmd(self, src: str) -> bool:
        """command としてそのまま送る。通れば環境が 1 つ進む。"""
        env_before = self.eng.env
        resp = self.guard(lambda: self.eng.send_cmd(src))
        if resp is None or panic_check(resp):
            return False

        advanced = not has_error(resp) and "env" in resp
        if advanced:
            self.eng.advance(resp)
            self.eng.log.append(src)
        render(resp, src)

        found = sorries(resp)
        self.clear_pending()
        if found and advanced:
            # 通らなかった宣言の sorry は持ち越さない。埋め戻しても同じ
            # エラーで弾かれるだけで、埋め戻しに失敗した直後に「sorry 1 個」
            # と出してから「sorry 2 個」と言い直すことになる。
            self.pending = found
            # env id を覚えておく。:undo などで環境が動いたら埋め戻しでは
            # 巻き戻さない (二重に pop して手前の宣言を落とすため)。
            self.proof_src = src
            self.sorry_env = self.eng.env
            print(dim(f"-- :prove で証明モードに入る (sorry {len(found)} 個)"))

        self.last = Last(src, advanced=advanced, env_before=env_before)
        return True

    def clear_pending(self) -> None:
        """sorry まわりの持ち越しを捨てる。環境が動いたら proofState は無効。"""
        self.pending = []
        self.proof_at = None
        self.proof_src, self.sorry_env = None, None

    def timing(self, t0: float) -> None:
        if self.show_time:
            print(dim(f"({time.time() - t0:.2f}s)"))

    # -- 証明モード -------------------------------------------------------

    def tactic(self, src: str) -> None:
        """1 行 = 1 タクティク。proofState を進める。"""
        proof = self.proof
        if proof is None:
            return

        before = proof.state
        gen = self.eng.gen
        resp = self.guard(lambda: self.eng.send_tactic(src, before))
        if self.proof is not proof or self.eng.gen != gen:
            # guard がエンジンを作り直した。手元の proofState は前のプロセスの
            # ものなので、返事が来ていても中身が違う。新しいエンジンは番号を
            # 0 から振り直すので、他の証明の状態に当たって「証明完了」まで
            # 出てしまう (宣言は sorry のまま残る)。
            self.drop_proof()
            return
        elif resp is None:
            return

        self.last = Last(src, advanced=False, proof=True)

        if resp.get("message"):  # エンジンからの素のエラー
            print(red(resp["message"].rstrip()))
            return
        elif has_error(resp):
            render(resp, src)
            return

        for m in messages(resp):
            if m.get("data"):
                print(m["data"].rstrip())

        if "proofState" not in resp:
            render(resp, src)
            return

        proof.stack.append(before)
        proof.state = resp["proofState"]

        found = try_this(messages(resp))
        if found and not balanced(found):
            # 提案を読み切れていない (メッセージの形が変わった等)。壊れた
            # 台本を完成した証明として出すより、打った通りを残す。
            print(dim("-- 提案を読み切れなかったので打った通りを台本に入れた"))
            found = None
        if found and found != src:
            shown = found.replace("\n", " ")
            print(dim(f"-- 台本には {shown} を入れた"))
        proof.script.append(found or src)
        self.last = Last(src, advanced=True, proof=True)

        goals = resp.get("goals") or []
        if goals:
            self.show_goals(goals)
            return

        script = "\n".join(proof.script)
        print(green("証明完了。"))
        print(dim("-- 台本:"))
        print(textwrap.indent(script, "  "))
        self.proof, self.last = None, None
        self.close_sorry(script)

    def drop_proof(self) -> None:
        """エンジンが作り直されたので証明モードを畳む。何が起きたかは言う。"""
        self.proof, self.last = None, None
        print(yellow("エンジンが変わったので証明モードを抜けた"))
        print(dim("  打っていたタクティクは通っていない"))
        if self.pending and self.eng.env is not None:
            # revive が replay で拾い直していれば、そのまま入り直せる。
            print(dim(f"  :prove で入り直せる (sorry {len(self.pending)} 個)"))
        else:
            # 作り直せなかったときの proofState は前のプロセスのもの。
            self.clear_pending()
            print(dim("  :restart で建て直してから打ち直す"))

    def close_sorry(self, script: str) -> None:
        """`by sorry` を台本で埋め戻して、宣言を本物として通し直す。"""
        src, at = self.proof_src, self.proof_at
        if not src or at is None:
            return

        new_src = splice_sorry(src, at, script)
        if new_src is None:
            # 位置が読めなかった。証明そのものは通っているので台本は上に
            # 出ている。黙って戻ると「証明完了」だけが残る。
            print(dim("-- 位置が読めなかったので宣言は sorry のまま"))
            return

        # sorry 版と同じ名前になるので、先に取り消してから通し直す。
        undone = None
        if self.sorry_env is not None and self.eng.env == self.sorry_env:
            undone = self.eng.pop_decl()

        # 通し直しに失敗したら戻せるよう控える。sorry が 2 個以上あるときに
        # 持ち越しを捨てると、残りを :prove で続けられなくなる。
        keep = (self.pending, self.proof_at, self.proof_src, self.sorry_env)
        self.proof_at, self.proof_src, self.sorry_env = None, None, None

        print(dim("-- 埋め戻して通す:"))
        print(textwrap.indent(new_src.strip(), "  "))

        gen = self.eng.gen
        self.submit(new_src, CMD)
        landed = bool(self.eng.log) and self.eng.log[-1] == new_src
        if undone is None or landed:
            # 着地したかは env の中身で決める。世代だけを見ると、落ちた
            # エンジンを guard が建て直して再送し**通った**ときにも
            # 「sorry のまま」と嘘をつき、sorry 版を流し直して重複エラーの
            # 宣言が控えに永久に居座る。
            return
        elif self.eng.gen != gen:
            # 建て直されて、そのうえ通らなかった。控えた env id は死んで
            # いるので据えず、sorry 版のテキストを流し直す。持ち越しは
            # 戻さない (死んだ proofState で replay_into が付け直した値を
            # 上書きすると、次の :prove が今の環境に無い状態を指す)。
            print(dim("-- エンジンが建て直されたので sorry のままにしておく"))
            if undone.src is not None:
                self.replay_into([undone.src])
        else:
            # 通らなかった。項の位置の sorry ではタクティクを差せない。
            self.eng.push_decl(undone)
            self.pending, self.proof_at, self.proof_src, self.sorry_env = keep
            print(dim("-- 通らなかったので sorry のままにしておく"))
            left = len(self.pending)
            if left > 1:
                print(dim(f"  残りは :prove <n> で続けられる (sorry {left} 個)"))

    def show_goals(self, goals: Sequence[str] | None = None) -> None:
        if goals is None:
            goals = self.proof.goals if self.proof else []
        if self.proof is not None:
            self.proof.goals = list(goals)

        for n, g in enumerate(goals):
            head = f"goal {n + 1}/{len(goals)}" if len(goals) > 1 else "goal"
            print(green(head))
            print(textwrap.indent(g, "  "))

    def show_pending(self) -> None:
        """証明モードの外での :goals。残っている sorry を出す。"""
        if not self.pending:
            print(dim("証明モードではない (sorry も残っていない)"))
            return

        for n, sy in enumerate(self.pending):
            print(green(f"sorry {n + 1} [proofState {sy.get('proofState')}]"))
            print(textwrap.indent((sy.get("goal") or "").rstrip(), "  "))

    def prove(self, arg: str) -> None:
        """直前の入力に出た sorry を 1 つ選んで証明モードに入る。"""
        found = self.pending
        if not found:
            print(red("直前の入力に sorry が無い"))
            return

        # isdigit は '²' に True を返すが int() は通らない。
        n = int(arg) - 1 if arg.isdecimal() else 0
        if not 0 <= n < len(found):
            print(red(f"sorry は {len(found)} 個。1..{len(found)} で指定する"))
            return

        sy = found[n]
        goal = sy.get("goal", "")
        self.proof = Proof(state=sy["proofState"], goals=[goal])
        self.proof_gen = self.eng.gen
        self.proof_at = sy  # 埋め戻すのはこの sorry。位置で切る
        print(dim(PROOF_HELP))
        self.show_goals([goal])

    # -- メタコマンド -----------------------------------------------------

    def meta(self, line: str) -> Step | None:
        """`:` で始まる行を捌く。"quit" を返したらループを抜ける。"""
        if line.startswith(":!"):
            try:
                subprocess.run(line[2:].strip(), shell=True, check=False)
            except KeyboardInterrupt:
                # 子は同じプロセスグループにいるので Ctrl-C はこちらにも来る。
                # 止めたいのは子だけ。ここで抜けると宣言を全部失う。
                print("^C")
            return None

        parts = line.split(None, 1)
        cmd = parts[0][1:]
        arg = parts[1].strip() if len(parts) > 1 else ""

        # 証明モード専用のものを先に見る。扱われなければ通常のコマンドとして続ける
        # (:t などは証明中でも使える)。
        if self.proof is not None and self.proof_meta(self.proof, cmd):
            return None

        match cmd:
            case "q" | "quit":
                return "quit"
            case "?" | "h" | "help":
                print(HELP, end="")
            case "goals" | "g":
                self.show_pending()
            case "script" | "done":
                print(dim("証明モードではない"))
            case "t" | "type":
                self.cmd_type(arg)
            case "i" | "info":
                self.cmd_info(arg)
            case "p" | "print":
                self.cmd_print(arg)
            case "l" | "load":
                self.load(arg)
            case "r" | "reload":
                if self.eng.loaded:
                    self.load(self.eng.loaded)
                else:
                    self.reset()
            case "reset":
                self.reset()
            case "undo":
                self.cmd_undo(arg)
            case "env":
                self.cmd_env(arg)
            case "prove":
                self.prove(arg)
            case "save":
                self.cmd_save(arg)
            case "time":
                self.show_time = not self.show_time
                print(dim(f"実行時間の表示: {'on' if self.show_time else 'off'}"))
            case "restart":
                self.cmd_restart()
            case _:
                print(red(f"不明なコマンド: :{cmd}  (:help)"))

        return None

    def proof_meta(self, proof: Proof, cmd: str) -> bool:
        """証明モード専用のコマンド。扱ったら True。"""
        match cmd:
            case "goals" | "g":
                self.show_goals()
            case "script":
                print(textwrap.indent("\n".join(proof.script) or "(空)", "  "))
            case "undo":
                self.proof_undo(proof)
            case "done" | "q" | "quit":
                self.proof, self.buf, self.ready = None, [], None
                print(dim("証明モードを出た"))
            case "?" | "h" | "help":
                print(PROOF_HELP)
            case _:
                return False

        return True

    def proof_undo(self, proof: Proof) -> None:
        if not proof.stack:
            print(dim("戻る先が無い"))
            return

        proof.state = proof.stack.pop()
        if proof.script:
            proof.script.pop()

        print(dim(f"proofState {proof.state}"))

    def cmd_type(self, arg: str) -> None:
        if not arg:
            print(red(":t には式が要る"))
            return

        out = self.guard(
            lambda: self.eng.query("#check\n" + textwrap.indent(arg, "  "))
        )
        print(out.rstrip() if out else red("型が取れなかった"))

    def cmd_info(self, arg: str) -> None:
        if not arg:
            print(red(":i には名前が要る"))
            return

        out = self.guard(lambda: self.eng.query(f"#check @{arg}"))
        print(out.rstrip() if out else red(f"不明: {arg}"))

        doc = self.guard(lambda: self.eng.query(DOC_QUERY % arg))
        if doc and doc.strip():
            print(dim(doc.strip()))

    def cmd_print(self, arg: str) -> None:
        if not arg:
            print(red(":p には名前が要る"))
            return

        resp = self.guard(lambda: self.eng.send_cmd(f"#print {arg}"))
        if resp is not None:
            render(resp, arg)

    def cmd_undo(self, arg: str) -> None:
        for _ in range(int(arg) if arg.isdecimal() else 1):
            if self.eng.stack:
                self.eng.env = self.eng.stack.pop()
            if self.eng.log:
                self.eng.log.pop()

        self.clear_pending()
        print(dim(f"env {self.eng.env}"))

    def cmd_restart(self) -> None:
        self.revive()

    def cmd_env(self, arg: str) -> None:
        """引数なしで今の環境、名前を渡すとその環境で起動し直す。"""
        if not arg:
            self.show_env()
            return
        elif arg == self.cfg.name:
            print(dim(f"すでに {arg}"))
            return

        try:
            target = resolve(name=arg)
        except ConfigError as e:
            print(red(str(e)))
            return

        try:
            why = problem(target)
            if why is None:
                prepare(target)  # 落とす前に用意まで済ませる
        except EngineError as e:
            why = str(e)

        if why:
            print(red(why))  # 今のエンジンは落とさない
            return

        keep, prev, back = self.show_time, self.eng.loaded, self.cfg
        # 打った宣言も控える。:l した中身は preload で戻るが、対話で打った
        # ぶんは新しい Engine には入っていない。戻り道で流し直す。
        # 環境に無い宣言も連れて行く。捨てると、直前に「テキストは控えてある」
        # と言ったものが :env で黙って消える。新しい環境なら通ることもある
        # (import が増える方向の切り替え)。通らなければまた控えに戻る。
        log = list(self.eng.log) + list(self.eng.unplayed)
        self.eng.kill()
        try:
            self._start(target, preload=prev)
        except START_FAILED as e:
            # import が通るかは boot するまで分からない。元の環境に戻す。
            print(red(f"{arg} で起動できなかった: {str(e) or '理由は分からない'}"))
            print(dim(f"  {back.name} に戻る"))
            try:
                self._start(back, preload=prev)
            except START_FAILED as back_e:
                die(f"{back.name} にも戻れなくなった: {back_e}")

        # 成功しても元に戻っても、対話で打った宣言は新しいエンジンには無い。
        # except の中だけで流していたので、切り替えが成功したときに限って
        # 打った宣言が黙って消えていた。
        self.replay_into(log)
        self.show_time = keep

    def show_env(self) -> None:
        print(dim(f"{self.cfg} — {self.cfg.project or 'Lake プロジェクト無し'}"))
        print(
            dim(
                f"env {self.eng.env} "
                f"(base {self.eng.base}, 宣言 {len(self.eng.log)} 件)"
            )
        )
        try:
            names = sorted(load_config().get("env") or {})
        except ConfigError as e:
            print(red(str(e)))
            return

        if names:
            print(dim(f"切り替え先: {' '.join(names)}  (:env <name>)"))

    def cmd_save(self, arg: str) -> None:
        """
        通した宣言を .lean として書き出す。

        repl には環境を pickle する機能もあるが、戻した環境で #eval すると
        Lean のコンパイラが PANIC する (コンパイラの状態が pickle に入らない)。
        ソースで持っておけば編集もできるし lean でそのまま走る。
        """
        if not arg:
            print(red(":save にはファイル名が要る"))
            return

        srcs = self.eng.sources()
        orphans = list(self.eng.unplayed)
        if not srcs and not orphans:
            print(red("保存する宣言が無い"))
            return

        path = os.path.abspath(os.path.expanduser(arg))
        if os.path.exists(path) and path not in self.saved:
            # 打ち間違いでプロジェクトのソースを潰さない。2 度目からは上書きする。
            print(red(f"すでにある: {path}"))
            print(dim("  消すか別の名前にする"))
            return

        parts = list(srcs)
        if orphans:
            # 環境に入らなかった宣言はコメントとして添える。落とすと :save は
            # 成功を報告したのに打ったものが消える。そのまま書けば lean で
            # 通らないファイルになる。
            note = "\n\n".join(textwrap.indent(one, "-- ") for one in orphans)
            parts.append(f"-- 環境に入らなかった宣言 ({len(orphans)} 件):\n{note}")

        body = "\n\n".join(parts)
        try:
            with open(path, "w") as f:
                # 起動と同じヘッダを書く。設定の import だけだと lean で
                # 直接通らない (leani は :l のときだけ import Lean を足す)。
                # :l したファイルの import も足す (save_header)。
                f.write(f"{self.eng.save_header()}\n{body}\n")
        except OSError as e:
            print(red(f"書き出せなかった: {e}"))
            return

        self.saved.add(path)
        print(dim(f"宣言 {len(srcs)} 件を書き出した: {path}"))
        print(dim(f"-- :l {path}"))

    def load(self, path: str, announce: bool = True) -> None:
        if not path:
            print(red("ファイル名が要る"))
            return

        path = os.path.expanduser(path)
        if not os.path.isfile(path):
            print(red(f"ファイルが無い: {path}"))
            return

        try:
            with open(path) as f:
                src = f.read()
        except OSError as e:
            print(red(f"読めない: {path} ({e.strerror})"))
            return

        left = len(self.eng.unplayed)  # 読み込みが通れば控えも作り直される
        out = self.guard(lambda: self.eng.load_file(path, src))
        if out is None:
            return

        render(out.resp, out.src)
        if out.note:
            print(yellow(out.note))
        if out.bad:
            print(red(f"読み込めなかった: {path}"))
        else:
            # 環境が総取り替えになるので、前の環境の proofState は使えない。
            self.proof, self.last, self.undone = None, None, None
            self.clear_pending()
            if announce:
                print(dim(f"読み込んだ: {path} (env {self.eng.env})"))
            if left:
                # :reset と同じで、黙って捨てると気付く場所が無い。
                print(dim(f"  環境に無かった宣言 {left} 件も捨てた"))

        self._comp_cache.clear()

    def reset(self) -> None:
        if self.eng.base is None:
            # 起点の環境が無い (boot が通らなかった)。据え直しても
            # "Unknown environment." しか返さない端末になり、控えだけが消える。
            print(red("起点の環境が無い。:restart で建て直す"))
            return

        self.eng.env, self.eng.stack, self.eng.log = self.eng.base, [], []
        # 控えも捨てる。残すと、:reset で消したはずの宣言が次の :restart で
        # 戻ってくる。
        left, self.eng.unplayed = len(self.eng.unplayed), []
        self.proof, self.last = None, None
        self.clear_pending()
        print(dim(f"env {self.eng.env} に戻した"))
        if left:
            print(dim(f"  環境に無かった宣言 {left} 件も捨てた"))
