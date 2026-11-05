"""フロント (副作用)。

入力を読み、送り方を決め、結果を表示する。入力バッファと証明モードを持つ。"""

from __future__ import annotations

import contextlib
import os
import shutil
import sys
import textwrap
import time
from collections.abc import Callable, Iterator, Sequence
from dataclasses import dataclass, replace
from typing import cast

from prompt_toolkit import PromptSession
from prompt_toolkit.application import get_app
from prompt_toolkit.buffer import Buffer
from prompt_toolkit.completion import (
    CompleteEvent,
    Completer,
    Completion,
    ExecutableCompleter,
    PathCompleter,
)
from prompt_toolkit.document import Document
from prompt_toolkit.filters import Condition
from prompt_toolkit.formatted_text import ANSI
from prompt_toolkit.history import FileHistory
from prompt_toolkit.key_binding import KeyBindings
from prompt_toolkit.key_binding.key_processor import KeyPressEvent
from prompt_toolkit.shortcuts import CompleteStyle

from leani.abbrev import (
    abbrev_candidates,
    abbrev_head,
    abbrev_lookup,
    abbrev_table,
    expand_abbrev,
)
from leani.boot import prepare
from leani.config import EnvConfig, load_config, problem, resolve
from leani.engine import Engine, Replay, Undone
from leani.places import COMPLETE_CAP, HIST, INIT, TTY
from leani.pure import (
    balanced,
    block_continues,
    c,
    cache_usable,
    classify,
    classify_tac,
    continues,
    dim,
    errors,
    green,
    grouped_tactic,
    has_error,
    head_line,
    indent_code,
    last_json,
    lean_str,
    lean_strs,
    loogle_text,
    messages,
    meta_names,
    name_chunk,
    name_start,
    nested_action,
    not_evaluable_reason,
    red,
    shorten,
    sorries,
    splice_sorry,
    strip_imports,
    tactic_parse_error,
    tactic_step,
    tactic_undo,
    try_this,
    yellow,
)
from leani.queries import (
    COMPLETE_QUERY,
    DECL_NAME,
    DOC_QUERY,
    META_LINE,
    PARSE_PROBE,
    SCOPE_QUERY,
)
from leani.search import loogle
from leani.show import Console, panic_check, render
from leani.types import (
    CMD,
    MORE,
    START_FAILED,
    TERM,
    ConfigError,
    EngineDied,
    EngineError,
    EnvLost,
    Interrupted,
    Kind,
    Loogle,
    Output,
    Probe,
    Proof,
    Scope,
    SearchError,
    Sorry,
    State,
    Step,
    T,
)


def abbrev_keys() -> KeyBindings:
    """
    space キーと Tab キーに略記の変換を割り当てる。

    space キーは変換したあとに space 自体も挿入する。変換のきっかけになった space を
    消費すると `a \\to b` が `a →b` になり、ユーザーは記号を入力するたびに space を
    追加で入力することになる。略記の表に無ければ何も変換しないので、space キーで
    空白が挿入されない場面は無い。

    Tab キーは空白を入れずに変換する。`(·.succ)` や `(\\to)` のように、記号のすぐ
    あとに文字を続けるときに使う。カーソルの手前が表にある `\\name` のときだけ
    変換し、それ以外では補完する (NameCompleter)。Lean の名前には `\\` が入らない
    ので、変換と名前の補完は、ユーザーが入力した文字で区別できる。Jupyter では、
    カーネルは space キーの入力を受け取れないので、Tab キーの補完だけで略記を
    置き換える。
    """
    kb = KeyBindings()

    def expand(buf: Buffer) -> None:
        got = expand_abbrev(buf.document.text_before_cursor)
        if got is None:
            return
        sym, back = got
        buf.delete_before_cursor(back)
        buf.insert_text(sym)

    @Condition
    def at_abbrev() -> bool:
        head = get_app().current_buffer.document.text_before_cursor
        return expand_abbrev(head) is not None

    @kb.add(" ")
    def _(event: KeyPressEvent) -> None:
        expand(event.current_buffer)
        event.current_buffer.insert_text(" ")

    @kb.add("tab", filter=at_abbrev)
    def _(event: KeyPressEvent) -> None:
        expand(event.current_buffer)

    return kb


HELP = """\
式を書くと #eval で評価する。宣言はそのまま実行する。入力が終わったかは Lean のパーサで
判定する。インデントした行か | で始まる行が続くあいだは入力を読み続け、空行で入力を
確定する。確定した直後にインデントした行を書くと、直前の入力の続きとして読み直す。
\\to のような略記は space か Tab で記号に変換する。Tab は記号のあとに空白を入れない。

  :t, :type <expr>    型を表示する (#check)
  :i, :info <name>    型と docstring を表示する
  :p, :print <name>   定義を表示する (#print)
  :loogle <q>         定理を検索する (外部サービスに問い合わせる)
                      例: :loogle |- ?a + ?b = ?b + ?a
                      名前、型、結論 (|- を付ける)、名前に含まれる語 ("commutative")
  :l, :load <file>    ファイルを読み込む (実行した宣言は捨てる)
  :r, :reload         ファイルを読み込み直す (読み込んでいなければ :reset)
  :reset              宣言をすべて取り消す (:l したあとは読み込んだ直後に戻る)
  :undo [n]           直前の n 件の宣言を取り消す
  :env [name]         今の環境を表示する / 設定した環境に切り替えて再起動する
  :prove [n]          n 番目の sorry の証明モードを始める
  :goals, :g          残っている sorry とゴールを表示する
  :save <file>        実行した宣言を .lean に書き出す (:l で読み込める)
  :time               実行時間の表示を切り替える
  :{ ... :}           複数行を明示的に囲む
  :! <cmd>            shell のコマンドを実行する
  :abbrev [s]         略記の一覧を表示する
                      s が記号ならその記号の略記、それ以外なら s で始まる略記
  :restart            エンジンを再起動して、宣言を再実行する
  :help, :h, :?       このヘルプを表示する
  :q, :quit           終了する (Ctrl-D)

証明モード (⊢>) では 1 回の入力が 1 タクティクになる。:goals :script :undo :done を
使える。証明モードの :help は、証明モードのコマンドだけを表示する。
"""

PROOF_HELP = (
    "証明モード: 1 回の入力が 1 タクティク。"
    ":goals ゴール  :script スクリプト  :undo 取り消す  :done 終了"
)

# Tab で補完するコマンド名。HELP から取り出すので、HELP に載せたものだけが候補になる。
META_NAMES = meta_names(HELP)


@dataclass(frozen=True)
class Held:
    """
    :prove を待っている sorry と、その sorry を含む宣言。
    """

    sorries: tuple[Sorry, ...]
    src: str  # sorry を含む宣言のソース
    env: int | None
    at: Sorry | None = None  # :prove で選んだ sorry (位置つき)


@dataclass(frozen=True)
class Last:
    """直前に送った入力。インデント行を受け取ったときに直前の入力に戻れるよう保存しておく。"""

    src: str
    advanced: bool  # 環境を進めたか (取り消すべきか)
    proof: bool = False  # 証明モードでのタクティクだったか
    env_before: int | None = None


# 再実行できなかった宣言は Engine.unplayed に保留する。その案内。
PENDING_HINT = "これらの宣言は環境に追加せず、保留にした。:restart で再実行できる"


class BlockHistory(FileHistory):
    """
    確定した入力を 1 件として保存する履歴。

    prompt_toolkit は prompt() から戻るたびにその 1 行を履歴に追加しようとするが、
    `def fib` の 4 行が 4 件になると、呼び出すのに Ctrl-P を 4 回押す必要がある。
    どこまでを 1 件とするかは leani 側が判断する (submit / discard) ので、
    prompt_toolkit による追加は無視して、record による追加だけを受け付ける。

    履歴ファイルには追記だけをする。読めない形式のファイル (readline や libedit の
    履歴) があっても、読めない行を無視するだけで、ファイルは上書きしない。
    """

    def append_string(self, string: str) -> None:
        pass

    def record(self, string: str) -> None:
        super().append_string(string)


class ShellCompleter(Completer):
    """
    `:!` の後を Tab キーで補完する。1 語目は PATH にあるコマンド、2 語目からは
    ファイルのパス。1 語目でも `/` を含んでいれば `./run.sh` のようなパスとして扱う。
    """

    def __init__(self) -> None:
        self.cmds = ExecutableCompleter()
        self.paths = PathCompleter(expanduser=True)

    def get_completions(
        self, document: Document, complete_event: CompleteEvent
    ) -> Iterator[Completion]:
        head, _, word = document.text_before_cursor.rpartition(" ")
        inner = self.cmds if head.strip() == "" and "/" not in word else self.paths
        # 候補はカーソルの前の語に対する位置で返るので、語だけを渡せばよい。
        yield from inner.get_completions(Document(word), complete_event)


def lean_or_dir(path: str) -> bool:
    """:l と :save で補完するパス。.lean ファイルとディレクトリだけにする。"""
    return path.endswith(".lean") or os.path.isdir(path)


def config_envs() -> list[str]:
    """設定ファイルに書いた環境の名前。設定ファイルを読めなければ候補を出さない。"""
    try:
        return sorted(load_config().get("env") or {})
    except ConfigError:
        return []


def pick(words: Sequence[str], prefix: str) -> Iterator[Completion]:
    """words のうち prefix で始まるものを、prefix を置き換える候補にする。"""
    return (
        Completion(w, start_position=-len(prefix))
        for w in words
        if w.startswith(prefix)
    )


class NameCompleter(Completer):
    """
    Tab キーで補完する。ふだんは定数名を補完し、候補は Repl が返す。`:` で始まる行では
    コマンド名を補完し、そのあとはコマンドに合わせて補完する。`:l` と `:save` の後は
    ファイルのパス、`:env` の後は設定ファイルに書いた環境の名前、`:!` の後は
    ShellCompleter に任せる。

    カーソルの手前が `\\name` の途中なら、行のどこでも略記の記号を補完する。space
    キーがどこでも略記を変換するので、Tab キーも同じ範囲で変換する。
    """

    def __init__(
        self,
        names: Callable[[str], list[str]],
        envs: Callable[[], list[str]] = list,
    ) -> None:
        self.names = names
        self.envs = envs
        self.shell = ShellCompleter()
        self.files = PathCompleter(file_filter=lean_or_dir, expanduser=True)

    def get_completions(
        self, document: Document, complete_event: CompleteEvent
    ) -> Iterator[Completion]:
        text = document.text_before_cursor
        head, sep, arg = text.partition(" ")
        abbrev = abbrev_head(text)
        match (head, sep):
            case _ if abbrev is not None:
                for sym in abbrev_candidates(abbrev):
                    yield Completion(sym, start_position=-len(abbrev) - 1)
            case (h, _) if h.startswith(":!"):
                yield from self.shell.get_completions(
                    Document(text[2:]), complete_event
                )
            case (h, "") if h.startswith(":"):
                yield from pick(META_NAMES, h[1:])
            case (":l" | ":load" | ":save", _):
                yield from self.files.get_completions(
                    Document(arg.lstrip()), complete_event
                )
            case (":env", _):
                yield from pick(self.envs(), arg.lstrip())
            case _:
                prefix = text[name_start(text) :]
                for name in self.names(prefix):
                    yield Completion(name, start_position=-len(prefix))


class Repl:
    """
    端末との対話。副作用の層。

    入力を 1 行受け取り (feed)、完結したかをパーサに問い合わせ (probe)、エンジンに
    送り (submit)、結果を表示する (render)。判定と整形は import している純粋な
    関数に分けてあるので、このクラスは状態遷移と入出力だけを扱う。持っている状態は
    エンジン (eng)、入力バッファ (buf / ready / explicit / last / undone)、証明モード
    (proof / held)、補完のキャッシュ (_comp_cache / _own / _scope)、それに設定と
    表示の切り替え (cfg / show_time / saved)。
    """

    # 行編集のセッションは 1 つを再利用する。:env で _start を呼び直しても履歴は
    # 引き継ぐ。
    _session: PromptSession[str] | None = None
    _history: BlockHistory | None = None

    def __init__(
        self, cfg: EnvConfig, preload: str | None = None, out: Output | None = None
    ) -> None:
        # 表示はすべて out に出す。端末では Console、Jupyter では kernel の実装を渡す。
        self.out: Output = out or Console()
        self.saved: set[str] = set()  # :save で書き出したパス。上書きの判断に使う
        self._start(cfg, preload)

    def _start(self, cfg: EnvConfig, preload: str | None = None) -> None:
        """エンジンを起動して起点の環境を作る。:env の切り替えでも呼ぶ。"""
        self.cfg = cfg

        self.buf: list[str] = []
        self.ready: Kind | None = None  # 構文的に完結しているときの送り方
        self.last: Last | None = None
        self.explicit = False  # :{ ... :} の中か
        # rewind で取り消したもの。取り消した宣言と、その宣言の sorry の保留か、
        # タクティクを取り消す前の証明モードの状態
        self.undone: tuple[Undone, Held | None] | Proof | None = None
        self.show_time = False

        self.proof: Proof | None = None
        self.held: Held | None = None

        # 補完と履歴
        self._comp_cache: dict[str, tuple[tuple[int, int | None], list[str]]] = {}
        self._own: tuple[list[str] | None, list[str]] = (None, [])
        self._scope: tuple[tuple[int, int | None] | None, Scope] = (None, {})

        self.eng = Engine(cfg)
        if self.eng.warning is not None:
            self.out.write(yellow(self.eng.warning))
        try:
            self._setup_prompt()
            t0 = time.time()
            self.eng.boot()
        except BaseException:
            # Engine を作った時点で repl のプロセスは起動している。ここで例外を
            # 投げると呼び出し元が self.eng を差し替えるので、そのプロセスを終了
            # させる方法が無くなる (:env の切り替えに失敗するたびにプロセスが
            # 1 つ残る)。
            self.eng.kill()
            raise
        self.out.write(dim(f"leani: {self.eng.tc} / {cfg} / {time.time() - t0:.1f}s"))

        if os.path.isfile(INIT):
            self.apply_init()
        if preload:
            self.load(preload)

    def apply_init(self) -> None:
        """
        init ファイルを起動直後の環境で実行する。
        """
        try:
            with open(INIT) as f:
                text = f.read()
        except OSError as e:
            self.out.fail(red(f"{INIT} が読めない ({e.strerror})"))
            return

        src = strip_imports(text)
        if not src.strip():
            return

        resp = self.guard(lambda: self.eng.run_init(src))
        if resp is None:
            return
        elif has_error(resp):
            self.out.fail(red(f"{INIT} にエラーがある:"))
            render(self.out, resp, src)
            return
        else:
            self.out.write(dim(f"-- {INIT} を読み込んだ"))

    # -- 行編集 -----------------------------------------------------------

    def _setup_prompt(self) -> None:
        """
        prompt_toolkit のセッションを用意する。端末でなければセッションを作らない。

        補完は Repl に紐付くので、:env で _start を呼び直したらそのたびに completer を
        差し替える。セッションと履歴は再利用して、環境を切り替えても Ctrl-P で前の
        履歴を呼び出せるようにする。
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
                # Tab キーを押したときだけ補完する。入力のたびに補完すると、
                # Mathlib ではキーを 1 つ押すごとにエンジンへ問い合わせることになる。
                complete_while_typing=False,
                # 共通部分まで補完し、候補を下に並べる。READLINE_LIKE は
                # 候補の一覧を in_terminal (CPR の応答を待つ) で表示するので、
                # 端末が応答するまで何も表示されない。
                complete_style=CompleteStyle.MULTI_COLUMN,
            )

        Repl._session.completer = NameCompleter(self.complete_names, config_envs)

    def remember(self, src: str) -> None:
        """履歴に 1 件として追加する。複数行の宣言も全体で 1 件になる。"""
        if Repl._history is not None and src.strip():
            Repl._history.record(src.rstrip())

    # -- 補完 -------------------------------------------------------------

    def complete_names(self, prefix: str) -> list[str]:
        """
        今の環境で短い名前で書ける定数のうち、prefix で始まるものを返す。
        """
        if len(prefix) < 2:
            return []

        scope = self._scope_now()
        opens = [("", list[str]())] + [(ns, hid) for ns, hid in scope.get("open", [])]
        full = {ns: f"{ns}.{prefix}" if ns else prefix for ns, _ in opens}
        chunks = self._chunks({name_chunk(f) for f in full.values()})

        hits: set[str] = set()
        for ns, hidden in opens:
            got = chunks.get(name_chunk(full[ns]))
            if got is not None:
                hits.update(shorten(got, prefix, ns, hidden))
        hits.update(a for a, _ in scope.get("alias", []) if a.startswith(prefix))
        hits.update(x for x in self._own_names() if x.startswith(prefix))
        return sorted(hits)

    def _chunks(self, keys: set[str]) -> dict[str, list[str]]:
        """
        名前空間ごとの定数名の一覧を返す。キャッシュに無いものは、定数を 1 回
        走査してまとめて取得する。
        """
        now = (self.eng.gen, self.eng.env)

        def usable(k: str) -> bool:
            # now ではなく今の値で判定する。問い合わせの途中でエンジンが再起動すると、
            # now のキャッシュも使えなくなる。
            got = self._comp_cache.get(k)
            eng = self.eng
            return got is not None and cache_usable(got[0], eng.gen, eng.env, eng.stack)

        missing = sorted(k for k in keys if not usable(k))
        if missing:
            out = self.guard(
                lambda: self.eng.query(
                    COMPLETE_QUERY % (lean_strs(missing), COMPLETE_CAP)
                )
            )
            got = last_json(out)
            if isinstance(got, list) and len(got) == len(missing):
                for k, names in zip(missing, got, strict=True):
                    self._comp_cache[k] = (now, names)

        return {k: self._comp_cache[k][1] for k in keys if usable(k)}

    def _scope_now(self) -> Scope:
        """
        今の namespace と open を返す。環境が変わったときだけ問い合わせ直す。
        """
        key = (self.eng.gen, self.eng.env)
        if self._scope[0] != key:
            out = self.guard(lambda: self.eng.query(SCOPE_QUERY))
            got = last_json(out)
            self._scope = (key, cast(Scope, got) if isinstance(got, dict) else {})

        return self._scope[1]

    def _own_names(self) -> list[str]:
        """
        REPL で実行した宣言の名前を返す。ログが変わったときだけ集め直す。

        長さだけを覚えると、`:undo` のあとに同じ数の宣言を足したときに気づかない。
        """
        if self._own[0] != self.eng.log:
            names: set[str] = set()
            for src in self.eng.log:
                names.update(DECL_NAME.findall(src))
            self._own = (list(self.eng.log), sorted(names))

        return self._own[1]

    # -- エンジンの管理 ---------------------------------------------------

    def revive(self) -> Replay | None:
        """
        エンジンを再起動して宣言を replay する。再起動できなければ None を返す。

        boot が失敗する (import に問題がある / Ctrl-C で中断した) ことはよくある。
        宣言は Engine 側に残るので、ユーザーは原因を直してから :restart で再実行
        できる。ここで例外を投げるとセッションごと終了するので、例外は投げない。
        """
        try:
            out = self.eng.restart()
        except Interrupted:
            self.out.write(yellow("^C 再起動を中断した。:restart でやり直せる"))
            self.fold_proof()
            return None
        except (EngineDied, OSError) as e:
            self.out.fail(
                red(f"エンジンを再起動できなかった: {str(e) or '理由は分からない'}")
            )
            self.out.write(dim("  原因を直してから :restart"))
            self.fold_proof()
            return None

        self.proof, self.last = None, None
        self.clear_pending()
        self.out.write(dim(f"宣言 {len(out.done)} 件を再実行した (env {self.eng.env})"))
        self.report_replay(out)
        self.reattach(out)
        return out

    def fold_proof(self) -> None:
        """
        再起動に失敗したときの後始末。環境が無いので、直前の入力に戻ることもできない。
        """
        self.last = None
        if self.proof is not None:
            self.drop_proof()
        self.clear_pending()

    def report_replay(self, out: Replay) -> None:
        """
        replay で再実行できなかったものを報告する。表示しないと、ユーザーは気付けない。
        """
        for note in out.notes:
            self.out.write(yellow(note))
        for src in out.failed:
            self.out.write(yellow(f"再実行に失敗した宣言: {head_line(src)}"))
        if out.skipped:
            self.out.write(yellow(f"再実行しなかった宣言: {len(out.skipped)} 件"))
        if out.failed or out.skipped:
            self.out.write(dim(f"  {PENDING_HINT}"))

    def reattach(self, out: Replay) -> None:
        """
        replay で戻した宣言に sorry が残っていたら、:prove を使えるようにする。
        """
        self.held = None
        if not out.sorries or not out.done:
            return

        self.held = Held(tuple(out.sorries), out.done[-1], self.eng.env)
        self.out.write(
            dim(f"-- :prove で証明モードを再開できる (sorry {len(out.sorries)} 個)")
        )

    def replay_into(self, log: Sequence[str]) -> None:
        """
        入力した宣言を今のエンジンで再実行する。:env の切り替え後と、エンジンの
        再起動で env id が無効になった宣言をテキストから戻すときに使う。
        """
        if not log:
            return

        if self.eng.env is None:
            self.eng.replay(log)
            self.out.write(
                yellow(f"環境が無いので、宣言 {len(log)} 件を再実行できなかった")
            )
            self.out.write(dim(f"  {PENDING_HINT}"))
            return

        out = self.guard(lambda: self.eng.replay(log))
        if out is None:
            self.out.write(yellow(f"宣言 {len(log)} 件を再実行できなかった"))
            return

        self.out.write(dim(f"宣言 {len(out.done)} 件を再実行した (env {self.eng.env})"))
        self.report_replay(out)
        self.reattach(out)

    def guard(self, fn: Callable[[], T], on_dead: T | None = None) -> T | None:
        """
        エンジンが異常終了したり中断されたりしたら、再起動して replay する。

        異常終了した場合は、再起動したうえで fn を 1 回だけ再実行する。ユーザーが
        Ctrl-C で止めた場合は再実行しない (ユーザーは止めたいので)。
        """
        retry = False
        try:
            return fn()
        except Interrupted:
            self.out.fail(yellow("^C 中断した。エンジンを再起動する…"))
        except EngineDied:
            self.out.write(red("エンジンが異常終了した。再起動する…"))
            retry = True

        if self.revive() is None:
            return on_dead

        if retry:
            try:
                return fn()
            except Interrupted:
                self.out.fail(yellow("^C 中断した。エンジンを再起動する…"))
            except EngineDied:
                self.out.fail(
                    red(
                        "再実行してもエンジンが異常終了した。"
                        "この入力は実行せずに、もう一度再起動する…"
                    )
                )
            self.revive()

        return on_dead

    # -- 完結判定 ---------------------------------------------------------

    def parse(self, src: str) -> Probe | None:
        """
        src が command、term、タクティクの列として読めるかを Lean のパーサに
        問い合わせる。1 回のやりとりで 3 つとも取得する。実行はしない。ユーザー定義の
        notation も認識される。
        """
        got = last_json(self.guard(lambda: self.eng.query(PARSE_PROBE % lean_str(src))))
        return cast(Probe, got) if isinstance(got, dict) else None

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
        1 行読む。Ctrl-C は KeyboardInterrupt、Ctrl-D は EOFError として呼び出し元に
        伝わる。

        prompt_toolkit が端末を制御するのは prompt() の中だけなので、評価中の Ctrl-C
        はこれまでどおり SIGINT として届く (Engine.send が Interrupted に変換する)。
        端末でなければセッションを作らないので、パイプ入力は標準の input() で読む。
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
                    self.out.write()
                    continue
                else:
                    self.out.write()
                    return 0
            except KeyboardInterrupt:
                self.discard()
                self.last = None
                self.out.write("^C")
                continue
            except UnicodeDecodeError as e:
                # main が errors="replace" に設定し直すのは tty でない stdin だけ。
                # stdin が端末で stdout が端末でないとき (`leani | tee log` など) は
                # input() が strict のまま読むので、この例外が起きる。ここでループを
                # 終了すると、それまでに実行した宣言も失われる。その行だけを捨てる。
                self.out.fail(red(f"UTF-8 として読めない行を飛ばした ({e.reason})"))
                continue

            try:
                if self.feed_line(line) == "quit":
                    return 0
            except KeyboardInterrupt:
                # Engine.send の中で押された Ctrl-C は Interrupted に変換されるが、
                # その外 (子プロセス、整形、補完) で押された Ctrl-C はここで捕まえる。
                self.discard()
                self.last = None
                self.out.write("^C")
            except EnvLost:
                # 環境が 1 つも無いので、続けてもどの入力もエラーになる。
                # cli が終了する。
                raise
            except Exception as e:
                self.out.fail(red(f"内部エラー: {type(e).__name__}: {e}"))
                self.discard()

    def feed_line(self, line: str) -> Step | None:
        """端末から受け取った 1 件を処理する。履歴から呼び出した入力は改行を含む。"""
        lines = line.split("\n")
        for one in lines:
            if self.feed(one) == "quit":
                return "quit"

        if len(lines) > 1 and self.ready is not None:
            return self.feed("")
        else:
            return None

    def discard(self) -> None:
        """入力中のブロックを捨てる。Ctrl-C / Ctrl-D で呼ぶ。"""
        self.remember("\n".join(self.buf))
        self.buf, self.ready, self.explicit = [], None, False
        self.restore_undone()

    def start_block(self) -> None:
        """
        ノートブックのセルの始まり。前のセルの宣言に、このセルのインデント行を
        続けないようにする。
        """
        self.last = None

    def end_block(self) -> None:
        """
        ノートブックのセルの終わりで入力を確定する。端末で空行を入力したときと同じ。
        :{ が閉じていなければ、:{ から後ろを捨てて、そのことを表示する。
        """
        if self.explicit:
            self.out.fail(red(":} が無いので、:{ から後ろは実行しなかった"))
            self.discard()
        elif self.buf:
            self.feed("")

    def close(self) -> None:
        """エンジンを終了させる。"""
        self.eng.kill()

    def feed(self, line: str) -> Step | None:
        """
        1 行受け取る。"quit" を返したらループを抜ける。
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
            # probe の最中に guard がエンジンの異常終了を検出し、revive が証明モードを
            # 終了した。このまま submit すると self.proof が None なので command として
            # 処理され、タクティクの行が宣言として送られて
            # "unexpected identifier; expected command" になる。さらに
            # submit_cmd の clear_pending が reattach で設定した値を消すので、直前に
            # 表示した「:prove で再開できる」も正しくなくなる。入力した行は送らずに
            # 捨て、案内だけを表示する。
            self.remember("\n".join(self.buf))
            self.buf, self.ready = [], None
            self.out.write(dim("  入力した行は実行しなかった"))
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
        """:{ ... :} の中の行。:} を受け取るまで判定せずに buf に追加する。"""
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
            self.rewind()
            self.buf = [*self.last.src.splitlines(), line]
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
        elif META_LINE.match(line):  # ブロックの入力をやめる
            src, kind = "\n".join(self.buf), self.ready
            self.buf, self.ready = [], None
            if kind is not None:
                self.submit(src, kind)
            else:
                self.remember(src)
                self.restore_undone()
                self.out.write(dim("-- 入力が途中だったので捨てた"))
                self.last = None
            return "quit" if self.feed(line) == "quit" else "done"
        elif self.ready is not None and not continues(line):
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
            if self.proof is None:
                return
            self.undone = self.proof
            self.proof = tactic_undo(self.proof) or self.proof
            return
        else:
            # 書き直しをやめたときに戻せるよう保存しておく
            # (Ctrl-C / Ctrl-D / ブロックの入力をやめたとき)。取り消した宣言の
            # sorry の保留も、今の環境のものではなくなるので一緒に保存する。
            self.undone = (self.eng.pop_decl(), self.held)
            self.held = None

    def restore_undone(self) -> None:
        """rewind で取り消したものを戻す。書き直さずにやめたとき。"""
        saved, self.undone = self.undone, None
        if saved is None:
            return
        if isinstance(saved, Proof):
            if saved.gen == self.eng.gen and self.proof is not None:
                self.proof = saved
            return

        undone, held = saved
        if self.eng.push_decl(undone):
            self.held = held
            return
        if undone.src is not None:
            self.replay_into([undone.src])

    # -- 送信 -------------------------------------------------------------

    def submit(self, src: str, kind: Kind | None = None) -> None:
        """完結した入力を送って結果を出す。"""
        self.remember(src)

        if self.eng.env is None:
            self.out.fail(red("エンジンが使えない。:restart で再起動できる"))
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
        is_do, head, col = src.lstrip().startswith("do"), "", 2
        resp = self.guard(lambda: self.eng.send_cmd("#eval\n" + indent_code(src, "  ")))
        if resp is None or panic_check(self.out, resp):
            return False

        errs = errors(resp)
        blob = "\n".join(m.get("data", "") for m in errs)

        if errs and not is_do and nested_action(blob):
            is_do, head, col = True, " do", 4
            resp = self.guard(
                lambda: self.eng.send_cmd(
                    f"#eval{head}\n" + indent_code(src, " " * col)
                )
            )
            if resp is None or panic_check(self.out, resp):
                return False
            errs = errors(resp)
            blob = "\n".join(m.get("data", "") for m in errs)

        if errs and is_do and "BaseIO" in blob:
            retry = f"#eval show IO _ from{head}\n" + indent_code(src, " " * col)
            again = self.guard(lambda: self.eng.send_cmd(retry))
            if again is not None and panic_check(self.out, again):
                return False
            elif again is not None and not has_error(again):
                render(self.out, again, src, line_off=1, col_off=col)
                self.last = Last(src, advanced=False)
                return True

        reason = not_evaluable_reason(blob) if errs else None
        if reason is not None:
            out = self.guard(
                lambda: self.eng.query(f"#check{head}\n" + indent_code(src, " " * col))
            )
            if out:
                self.out.write(out.rstrip())
                self.out.write(dim(f"-- {reason}。型だけを表示した"))
                self.last = Last(src, advanced=False)
                return True

        render(self.out, resp, src, line_off=1, col_off=col)
        self.last = Last(src, advanced=False)
        return True

    def submit_cmd(self, src: str) -> bool:
        """command としてそのまま送る。エラーが無ければ環境を 1 つ進める。"""
        env_before = self.eng.env
        resp = self.guard(lambda: self.eng.send_cmd(src))
        if resp is None or panic_check(self.out, resp):
            return False

        advanced = not has_error(resp) and "env" in resp
        if advanced:
            self.eng.accept(resp, src)
        render(self.out, resp, src)

        found = sorries(resp)
        self.clear_pending()
        if found and advanced:
            self.held = Held(tuple(found), src, self.eng.env)
            self.out.write(
                dim(f"-- :prove で証明モードを始められる (sorry {len(found)} 個)")
            )

        self.last = Last(src, advanced=advanced, env_before=env_before)
        return True

    def clear_pending(self) -> None:
        """sorry に関する保留中の状態を捨てる。環境が変わると proofState は無効。"""
        self.held = None

    def timing(self, t0: float) -> None:
        if self.show_time:
            self.out.write(dim(f"({time.time() - t0:.2f}s)"))

    # -- 証明モード -------------------------------------------------------

    def tactic(self, src: str) -> None:
        """1 回の入力 = 1 タクティク。proofState を進める。"""
        proof = self.proof
        if proof is None:
            return

        state = proof.state
        resp = self.guard(lambda: self.eng.send_tactic(grouped_tactic(src), state))
        if self.proof is not proof or proof.gen != self.eng.gen:
            self.drop_proof()
            return
        elif resp is None or panic_check(self.out, resp):
            return

        self.last = Last(src, advanced=False, proof=True)

        if resp.get("message"):  # エンジンが直接返すエラー
            shown = tactic_parse_error(resp["message"], self.parse(src))
            self.out.fail(red(shown.rstrip()))
            return
        elif has_error(resp):
            render(self.out, resp, src, 1)
            return

        for m in messages(resp):
            if m.get("data"):
                self.out.write(m["data"].rstrip())

        if "proofState" not in resp:
            render(self.out, resp, src, 1)
            return

        found = try_this(messages(resp))
        if found and not balanced(found):
            self.out.write(
                dim(
                    "-- 提案を読み取れなかったので、"
                    "入力したタクティクをそのままスクリプトに記録した"
                )
            )
            found = None
        if found and found != src:
            shown = found.replace("\n", " ")
            self.out.write(dim(f"-- スクリプトには {shown} を記録した"))
        goals = resp.get("goals") or []
        proof = tactic_step(proof, resp["proofState"], goals, found or src)
        self.proof = proof
        self.last = Last(src, advanced=True, proof=True)

        if goals:
            self.show_goals(goals)
            return

        script = "\n".join(proof.script)
        self.out.write(green("証明完了。"))
        self.out.write(dim("-- スクリプト:"))
        self.out.write(indent_code(script, "  "))
        self.proof, self.last = None, None
        self.close_sorry(script)

    def drop_proof(self) -> None:
        """
        エンジンが再起動されたか、再起動に失敗したので、証明モードを終了する。
        何が起きたかはユーザーに表示する。
        """
        self.proof, self.last = None, None
        self.out.write(
            yellow("証明していたエンジンが終了したので、証明モードを終了した")
        )
        self.out.write(
            dim("  入力したタクティクは証明に反映されていない (宣言は sorry のまま)")
        )
        if self.held is not None and self.eng.env is not None:
            left = len(self.held.sorries)
            self.out.write(dim(f"  :prove で再開できる (sorry {left} 個)"))
        else:
            self.clear_pending()
            self.out.write(dim("  :restart で再起動してから入力し直す"))

    def close_sorry(self, script: str) -> None:
        """:prove で選んだ sorry をスクリプトで置き換えて、宣言を再実行する。"""
        held = self.held
        if held is None or held.at is None:
            return
        src, at = held.src, held.at

        new_src = splice_sorry(src, at, script)
        if new_src is None:
            self.out.write(
                dim("-- sorry の位置を読み取れなかったので、宣言は sorry のままにする")
            )
            return

        undone = None
        if held.env is not None and self.eng.env == held.env:
            undone = self.eng.pop_decl()

        self.held = None

        self.out.write(dim("-- sorry をスクリプトで置き換えて実行する:"))
        self.out.write(indent_code(new_src.strip(), "  "))

        gen = self.eng.gen
        self.submit(new_src, CMD)
        landed = bool(self.eng.log) and self.eng.log[-1] == new_src
        if undone is None or landed:
            return
        elif self.eng.gen != gen:
            self.out.write(dim("-- エンジンを再起動したので、sorry のままにしておく"))
            if undone.src is not None:
                self.replay_into([undone.src])
        else:
            self.eng.push_decl(undone)
            self.held = held
            self.out.write(dim("-- エラーになったので、sorry のままにしておく"))
            left = len(held.sorries)
            if left > 1:
                self.out.write(
                    dim(f"  残りは :prove <n> で続けられる (sorry {left} 個)")
                )

    def show_goals(self, goals: Sequence[str] | None = None) -> None:
        if goals is None:
            goals = self.proof.goals if self.proof else ()

        for n, g in enumerate(goals):
            head = f"goal {n + 1}/{len(goals)}" if len(goals) > 1 else "goal"
            self.out.write(green(head))
            self.out.write(textwrap.indent(g, "  "))

    def show_pending(self) -> None:
        """証明モードの外での :goals。残っている sorry を出す。"""
        if self.held is None:
            self.out.write(dim("証明モードではない (sorry も残っていない)"))
            return

        for n, sy in enumerate(self.held.sorries):
            self.out.write(green(f"sorry {n + 1} [proofState {sy.get('proofState')}]"))
            self.out.write(textwrap.indent((sy.get("goal") or "").rstrip(), "  "))

    def prove(self, arg: str) -> None:
        """直前の入力に出た sorry を 1 つ選んで証明モードを始める。"""
        held = self.held
        if held is None:
            self.out.fail(red("直前の入力に sorry が無い"))
            return
        found = held.sorries

        # isdigit は '²' に True を返すが、int() は '²' を変換できない。
        n = int(arg) - 1 if arg.isdecimal() else 0
        if not 0 <= n < len(found):
            self.out.fail(red(f"sorry は {len(found)} 個。1..{len(found)} で指定する"))
            return

        sy = found[n]
        goal = sy.get("goal", "")
        self.proof = Proof(state=sy["proofState"], goals=(goal,), gen=self.eng.gen)
        self.held = replace(held, at=sy)  # 置き換えるのはこの sorry。位置で特定する
        self.out.write(dim(PROOF_HELP))
        self.show_goals([goal])

    # -- メタコマンド -----------------------------------------------------

    def meta(self, line: str) -> Step | None:
        """`:` で始まる行を処理する。"quit" を返したらループを抜ける。"""
        if line.startswith(":!"):
            try:
                self.out.shell(line[2:].strip())
            except KeyboardInterrupt:
                # 子プロセスは同じプロセスグループにいるので、Ctrl-C の SIGINT は
                # leani にも届く。止めたいのは子プロセスだけ。ここでループを抜けると
                # 宣言をすべて失う。
                self.out.write("^C")
            return None

        parts = line.split(None, 1)
        cmd = parts[0][1:]
        arg = parts[1].strip() if len(parts) > 1 else ""

        if self.proof is not None and self.proof_meta(self.proof, cmd):
            return None

        match cmd:
            case "q" | "quit":
                return "quit"
            case "?" | "h" | "help":
                self.out.write(HELP, end="")
            case "goals" | "g":
                self.show_pending()
            case "script" | "done":
                self.out.write(dim("証明モードではない"))
            case "t" | "type":
                self.cmd_type(arg)
            case "i" | "info":
                self.cmd_info(arg)
            case "p" | "print":
                self.cmd_print(arg)
            case "loogle":
                self.cmd_loogle(arg)
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
                self.out.write(
                    dim(f"実行時間の表示: {'on' if self.show_time else 'off'}")
                )
            case "restart":
                self.cmd_restart()
            case "abbrev":
                self.cmd_abbrev(arg)
            case _:
                self.out.fail(red(f"不明なコマンド: :{cmd}  (:help)"))

        return None

    def proof_meta(self, proof: Proof, cmd: str) -> bool:
        """証明モード専用のコマンド。扱ったら True。"""
        match cmd:
            case "goals" | "g":
                self.show_goals()
            case "script":
                self.out.write(indent_code("\n".join(proof.script) or "(空)", "  "))
            case "undo":
                self.proof_undo(proof)
            case "done" | "q" | "quit":
                self.proof, self.buf, self.ready = None, [], None
                self.out.write(dim("証明モードを終了した"))
            case "?" | "h" | "help":
                self.out.write(PROOF_HELP)
            case _:
                return False

        return True

    def proof_undo(self, proof: Proof) -> None:
        back = tactic_undo(proof)
        if back is None:
            self.out.write(dim("取り消せるタクティクが無い"))
            return

        self.proof = back
        self.out.write(dim(f"proofState {back.state}"))

    def cmd_abbrev(self, arg: str) -> None:
        pairs = abbrev_lookup(arg)
        if pairs:
            width = shutil.get_terminal_size().columns
            self.out.page(abbrev_table(pairs, width))
        else:
            what = "で始まる" if arg.isascii() else "を入力する"
            self.out.write(dim(f"{arg} {what}略記は無い"))

    def cmd_type(self, arg: str) -> None:
        if not arg:
            self.out.fail(red(":t には式が必要"))
            return

        out = self.guard(lambda: self.eng.query("#check\n" + indent_code(arg, "  ")))
        if out:
            self.out.write(out.rstrip())
        else:
            self.out.fail(red("型を取得できなかった"))

    def cmd_info(self, arg: str) -> None:
        if not arg:
            self.out.fail(red(":i には名前が必要"))
            return

        out = self.guard(lambda: self.eng.query(f"#check @{arg}"))
        if out:
            self.out.write(out.rstrip())
        else:
            self.out.fail(red(f"不明: {arg}"))

        doc = self.guard(lambda: self.eng.query(DOC_QUERY % arg))
        if doc and doc.strip():
            self.out.write(dim(doc.strip()))

    def cmd_print(self, arg: str) -> None:
        if not arg:
            self.out.fail(red(":p には名前が必要"))
            return

        resp = self.guard(lambda: self.eng.send_cmd(f"#print {arg}"))
        if resp is not None:
            render(self.out, resp, arg)

    def cmd_loogle(self, arg: str) -> None:
        """loogle に問い合わせる。エンジンは操作しないので、環境は変わらない。"""
        if not arg:
            self.out.fail(
                red(":loogle には名前か型のパターンが必要  (例: |- ?a + ?b = ?b + ?a)")
            )
            return

        try:
            got = self.ask_loogle(arg)
        except SearchError as e:
            self.out.fail(red(f"loogle の検索に失敗した: {e}"))
            return

        self.out.write(loogle_text(got, width=shutil.get_terminal_size().columns))

    def ask_loogle(self, query: str) -> Loogle:
        """
        loogle に問い合わせているあいだ、メッセージを 1 行表示する。
        """
        if not TTY:
            return loogle(query)

        self.out.write(dim("loogle に問い合わせている…"), end="")
        try:
            return loogle(query)
        finally:
            self.out.write("\r\033[K", end="")

    def cmd_undo(self, arg: str) -> None:
        for _ in range(int(arg) if arg.isdecimal() else 1):
            self.eng.pop_decl()

        self.clear_pending()
        self.out.write(dim(f"env {self.eng.env}"))

    def cmd_restart(self) -> None:
        self.revive()

    def cmd_env(self, arg: str) -> None:
        """引数が無ければ今の環境を表示し、名前を渡すとその環境で再起動する。"""
        if not arg:
            self.show_env()
            return
        elif arg == self.cfg.name:
            self.out.write(dim(f"すでに {arg}"))
            return

        try:
            target = resolve(name=arg)
        except ConfigError as e:
            self.out.fail(red(str(e)))
            return

        try:
            why = problem(target)
            if why is None:
                prepare(target)  # 今のエンジンを終了させる前に準備を済ませる
        except EngineError as e:
            why = str(e)

        if why:
            self.out.fail(red(why))  # 今のエンジンは終了させない
            return

        keep, prev, back = self.show_time, self.eng.loaded, self.cfg
        log = list(self.eng.log) + list(self.eng.unplayed)
        self.eng.kill()
        try:
            self._start(target, preload=prev)
        except START_FAILED as e:
            self.out.fail(
                red(f"{arg} で起動できなかった: {str(e) or '理由は分からない'}")
            )
            self.out.write(dim(f"  {back.name} に戻る"))
            try:
                self._start(back, preload=prev)
            except START_FAILED as back_e:
                raise EnvLost(f"{back.name} にも戻れなくなった: {back_e}") from back_e

        self.replay_into(log)
        self.show_time = keep

    def show_env(self) -> None:
        self.out.write(
            dim(f"{self.cfg}: {self.cfg.project or 'Lake プロジェクト無し'}")
        )
        self.out.write(
            dim(
                f"env {self.eng.env} "
                f"(base {self.eng.base}, 宣言 {len(self.eng.log)} 件)"
            )
        )
        try:
            names = sorted(load_config().get("env") or {})
        except ConfigError as e:
            self.out.fail(red(str(e)))
            return

        if names:
            self.out.write(dim(f"切り替え先: {' '.join(names)}  (:env <name>)"))

    def cmd_save(self, arg: str) -> None:
        """
        実行した宣言を .lean ファイルとして書き出す。
        """
        if not arg:
            self.out.fail(red(":save にはファイル名が必要"))
            return

        srcs = self.eng.sources()
        orphans = list(self.eng.unplayed)
        if not srcs and not orphans:
            self.out.fail(red("保存する宣言が無い"))
            return

        path = os.path.abspath(os.path.expanduser(arg))
        if os.path.exists(path) and path not in self.saved:
            # パスの入力を間違えて、プロジェクトのソースを上書きしない。同じパスへの
            # 2 回目以降の :save は上書きする。
            self.out.fail(red(f"すでにある: {path}"))
            self.out.write(dim("  ファイルを消すか、別の名前を指定する"))
            return

        parts = list(srcs)
        if orphans:
            # 環境に追加されなかった宣言はコメントとして書き出す。省くと、:save は
            # 成功を報告したのに入力した宣言が消える。コメントにせずそのまま書くと、
            # lean でエラーになるファイルになる。
            note = "\n\n".join(textwrap.indent(one, "-- ") for one in orphans)
            parts.append(f"-- 保留中の宣言 ({len(orphans)} 件):\n{note}")

        body = "\n\n".join(parts)
        try:
            with open(path, "w") as f:
                f.write(f"{self.eng.save_header()}\n{body}\n")
        except OSError as e:
            self.out.fail(red(f"書き出せなかった: {e}"))
            return

        self.saved.add(path)
        self.out.write(dim(f"宣言 {len(srcs)} 件を書き出した: {path}"))
        self.out.write(dim(f"-- :l {path}"))

    def load(self, path: str, announce: bool = True) -> None:
        if not path:
            self.out.fail(red("ファイル名が必要"))
            return

        path = os.path.expanduser(path)
        if not os.path.isfile(path):
            self.out.fail(red(f"ファイルが無い: {path}"))
            return

        try:
            with open(path) as f:
                src = f.read()
        except OSError as e:
            self.out.fail(red(f"読めない: {path} ({e.strerror})"))
            return

        left = len(self.eng.unplayed)  # 読み込みが成功すると保留は捨てられる
        out = self.guard(lambda: self.eng.load_file(path, src))
        if out is None:
            return

        render(self.out, out.resp, out.src)
        if out.note:
            self.out.write(yellow(out.note))
        if out.bad:
            self.out.fail(red(f"読み込めなかった: {path}"))
        else:
            self.proof, self.last, self.undone = None, None, None
            self.clear_pending()
            if announce:
                self.out.write(dim(f"読み込んだ: {path} (env {self.eng.env})"))
            if left:
                self.out.write(dim(f"  保留中の宣言 {left} 件も捨てた"))

        self._comp_cache.clear()

    def reset(self) -> None:
        if self.eng.base is None:
            self.out.fail(red("元になる環境が無い。:restart で再起動できる"))
            return

        left = self.eng.reset()
        self.proof, self.last = None, None
        self.clear_pending()
        self.out.write(dim(f"env {self.eng.env} に戻した"))
        if left:
            self.out.write(dim(f"  保留中の宣言 {left} 件も捨てた"))
