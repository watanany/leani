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
from prompt_toolkit.completion import (
    CompleteEvent,
    Completer,
    Completion,
    ExecutableCompleter,
    PathCompleter,
)
from prompt_toolkit.document import Document
from prompt_toolkit.formatted_text import ANSI
from prompt_toolkit.history import FileHistory
from prompt_toolkit.key_binding import KeyBindings
from prompt_toolkit.key_binding.key_processor import KeyPressEvent
from prompt_toolkit.shortcuts import CompleteStyle

from leani.abbrev import abbrev_lookup, abbrev_table, expand_abbrev
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
    space キーに略記の変換を割り当てる。

    Tab キーは補完だけに使う。端末では space で変換できるので、Tab キーにも変換を
    割り当てる必要が無い。Jupyter では、カーネルは space キーの入力を受け取れない
    ので、Tab キーで変換する (kernel の do_complete)。どちらでも、変換になるのは
    カーソルの前に `\\` があるときだけで、Lean の名前には `\\` が入らないので、
    名前の補完と区別できる。

    space 自体もそのまま挿入する。変換のきっかけになった space を消費すると
    `a \\to b` が `a →b` になり、ユーザーは記号を入力するたびに space を追加で
    入力することになる。略記の表に無ければ何も変換しないので、space キーで空白が
    挿入されない場面は無い。
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
式を書くと #eval で評価する。宣言はそのまま実行する。入力が終わったかは Lean のパーサで
判定する。インデントした行か | で始まる行が続くあいだは入力を読み続け、空行で入力を
確定する。確定した直後にインデントした行を書くと、直前の入力の続きとして読み直す。

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

    保留するのは直前の宣言の sorry だけである。環境を変える操作は、保留を捨てるか、
    今の環境の宣言の sorry で作り直す。
    """

    sorries: tuple[Sorry, ...]
    src: str  # sorry を含む宣言のソース
    # その宣言が作った env id。:undo などで環境が変わっていたら、sorry を置き換える
    # ときに宣言を取り消さない (二重に pop して手前の宣言が消えるため)。
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
        match (head, sep):
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

        # 入力バッファ
        self.buf: list[str] = []
        self.ready: Kind | None = None  # 構文的に完結しているときの送り方
        self.last: Last | None = None
        self.explicit = False  # :{ ... :} の中か
        # rewind で取り消した宣言と、その宣言の sorry の保留
        self.undone: tuple[Undone, Held | None] | None = None
        self.show_time = False

        # 証明モード
        self.proof: Proof | None = None
        self.held: Held | None = None

        # 補完と履歴
        self._comp_cache: dict[tuple[int | None, str], list[str]] = {}
        self._own: tuple[int, list[str]] = (-1, [])
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

        :l と違って環境を作り直さない。ここで実行した宣言は base に含めるので
        :reset しても消えない (GHCi の .ghci と同じ扱い)。
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

        `open Lean` のあとの `Json.pa` は `Lean.Json.pa` として探し、`Lean.` を
        取り除いて返す。namespace の中なら、その名前空間と親の名前空間からも同じ
        ように探す。
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

        キャッシュは base 環境 (import / :l 直後の環境) に紐付ける。宣言を 1 つ
        実行するたびにキャッシュを捨てると Mathlib では毎回 1.1 秒かかるので、
        REPL で実行した宣言の名前だけを Python 側で追加する。open する名前空間が
        増えても、定数の走査は 1 回で済ませる。
        """
        base = self.eng.base
        missing = sorted(k for k in keys if (base, k) not in self._comp_cache)
        if missing:
            out = self.guard(
                lambda: self.eng.query(
                    COMPLETE_QUERY % (lean_strs(missing), COMPLETE_CAP)
                )
            )
            got = last_json(out)
            if isinstance(got, list) and len(got) == len(missing):
                for k, names in zip(missing, got, strict=True):
                    self._comp_cache[(base, k)] = names

        return {
            k: self._comp_cache[(base, k)]
            for k in keys
            if (base, k) in self._comp_cache
        }

    def _scope_now(self) -> Scope:
        """
        今の namespace と open を返す。環境が変わったときだけ問い合わせ直す。

        open は宣言と同じく環境ごとに repl が保存しているので、入力した文字列から
        解析するより正確に取得できる (`open X in` は含まれず、`hiding` や
        `renaming` も反映される)。この問い合わせは定数を走査しないので、Mathlib
        でも数十ミリ秒で終わる。
        """
        key = (self.eng.gen, self.eng.env)
        if self._scope[0] != key:
            out = self.guard(lambda: self.eng.query(SCOPE_QUERY))
            got = last_json(out)
            self._scope = (key, cast(Scope, got) if isinstance(got, dict) else {})

        return self._scope[1]

    def _own_names(self) -> list[str]:
        """REPL で実行した宣言の名前を返す。ログが増えたときだけ集め直す。"""
        if self._own[0] != len(self.eng.log):
            names: set[str] = set()
            for src in self.eng.log:
                names.update(DECL_NAME.findall(src))
            self._own = (len(self.eng.log), sorted(names))

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

        再起動に失敗したときも証明モードを終了する。終了しないと、無効になった
        proofState を保持したままになり、leani はどの行にも赤い
        "Unknown proof state." だけを返す証明モードのままになる。
        """
        self.last = None
        if self.proof is not None:
            self.drop_proof()
        # 保留している proofState も終了したプロセスのもの。残すと :goals が
        # 環境に無い宣言のゴールを表示し、:prove がその無効な proofState で証明モードを
        # 始める。
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

        タクティクの実行中にエンジンが異常終了したときや Ctrl-C を押したとき、証明
        していた宣言は replay で環境に戻っている。新しい proofState を取得し直さないと、
        宣言はあるのに :prove が「sorry が無い」と表示するだけになり、ユーザーは
        :undo するしかなくなる。

        戻した最後の宣言に sorry が無ければ、保留も捨てる。submit_cmd と同じく、
        保留するのは直前の宣言の sorry だけである。残すと、前の宣言の proofState で
        :prove が証明モードを始める。
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
            # ここはユーザーに案内を表示する層。Engine のチェックで出る例外を
            # そのまま「内部エラー」として表示すると、テキストが残っていることも、
            # 次に何をすればよいかもユーザーに伝わらない。宣言を保留にするのは
            # Engine.replay に任せ、案内だけをここで表示する。
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

        再実行でもエンジンが終了したら、もう一度だけ再起動して、再実行はしない。
        同じ入力でエンジンが毎回異常終了する場合がある。再起動しないと、終了した
        プロセスが残り、次の入力もすべて失敗する。
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
        src が command / term / tacticSeq として読めるかを Lean のパーサに問い合わせる。
        1 回のやりとりで 3 つとも取得する。実行はしない。ユーザー定義の notation も
        認識される。
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
                # 想定外の例外でも、その 1 行のエラーとして扱う。セッションを終了すると
                # それまでの宣言をすべて失うので、それが一番大きな損失になる。
                self.out.fail(red(f"内部エラー: {type(e).__name__}: {e}"))
                self.discard()

    def feed_line(self, line: str) -> Step | None:
        """端末から受け取った 1 件を処理する。履歴から呼び出した入力は改行を含む。"""
        # 1 件にまとめた履歴を呼び出すと改行を含む 1 行として返ってくるので、
        # 行に分けて、入力したときと同じ順に feed へ渡す。
        lines = line.split("\n")
        for one in lines:
            if self.feed(one) == "quit":
                return "quit"

        # 呼び出した複数行は全体で 1 件なので、末尾に空行を渡して確定させる。
        waiting = bool(self.buf or self.explicit)
        if len(lines) > 1 and waiting:
            return self.feed("")
        else:
            return None

    def discard(self) -> None:
        """入力中のブロックを捨てる。Ctrl-C / Ctrl-D で呼ぶ。"""
        # 入力した内容は履歴に残す。捨てるのは入力バッファであって、入力の記録
        # ではない。長い宣言の入力を間違えたときに、ユーザーは Ctrl-P で呼び出せる。
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

        完結したかはパーサに問い合わせるが、それだけでは足りない。Lean では
        `structure P where` や `def f := 1` はそれ自体で完結した command なので、
        パーサは「完結した」と返す。それでも次のインデント行は続きになりうる。
        そこで次のように判定する:

        * パーサが「途中」と返したら継続行を読む (ブロックを始める)。
        * ブロックの中では、インデント行が続く限り読む。空行を受け取ったら確定する。
          パーサが「完結」と返したあとなら、インデントの無い行を受け取っても確定する。
        * 確定した直後にインデント行を受け取ったら、直前の入力の続きとして読み直す
          (環境も 1 つ戻す)。
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
            # 確定した入力の続きだった。直前の入力を取り消して、続きとして読み直す。
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
            # 確定済みのブロックのあとにインデントの無い行を受け取った。
            # ブロックを送ってから、その行を読み直す
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
            # 証明モードのタクティクは環境を進めていない。証明モードが終了していても、
            # ここで宣言を pop してはいけない (直前の実際の宣言が消える)。
            if self.proof is None:
                return
            self.proof = tactic_undo(self.proof) or self.proof
            return
        else:
            # 書き直しをやめたときに戻せるよう保存しておく
            # (Ctrl-C / Ctrl-D / ブロックの入力をやめたとき)。取り消した宣言の
            # sorry の保留も、今の環境のものではなくなるので一緒に保存する。
            self.undone = (self.eng.pop_decl(), self.held)
            self.held = None

    def restore_undone(self) -> None:
        """rewind で取り消した宣言を戻す。書き直さずにやめたとき。"""
        if self.undone is None:
            return

        (undone, held), self.undone = self.undone, None
        if self.eng.push_decl(undone):
            self.held = held
            return
        if undone.src is not None:
            # 保存しているあいだにエンジンが再起動された。保存した env id は無効に
            # なっているので設定できない。テキストから再実行する。
            self.replay_into([undone.src])

    # -- 送信 -------------------------------------------------------------

    def submit(self, src: str, kind: Kind | None = None) -> None:
        """完結した入力を送って結果を出す。"""
        # 送れるかを確認する前に履歴に追加する。エンジンが異常終了しているときこそ、
        # ユーザーが入力を呼び出せる必要がある。
        self.remember(src)

        if self.eng.env is None:
            # boot に失敗したエンジン。送れば send_cmd のチェックで止まるが、
            # ユーザーに必要なのは例外の名前ではなく次に何をすればよいかなので、
            # ここで案内を表示する。
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
        resp = self.guard(
            lambda: self.eng.send_cmd("#eval\n" + textwrap.indent(src, "  "))
        )
        if resp is None or panic_check(self.out, resp):
            return False

        errs = errors(resp)
        blob = "\n".join(m.get("data", "") for m in errs)

        # `(← e)` は do の中でしか書けないので、do の外では必ずエラーになる。
        # このエラーのときだけ do で包み直すので、ほかの入力の意味は変わらない。
        # 元の行の相対的なインデントを保つため、`do` は前の行に置く。
        if errs and not is_do and nested_action(blob):
            is_do, head, col = True, " do", 4
            resp = self.guard(
                lambda: self.eng.send_cmd(
                    f"#eval{head}\n" + textwrap.indent(src, " " * col)
                )
            )
            if resp is None or panic_check(self.out, resp):
                return False
            errs = errors(resp)
            blob = "\n".join(m.get("data", "") for m in errs)

        # `do` を単体で書くと、Lean は最初の action からモナドを決めてしまう
        # (IO.getEnv なら BaseIO)。GHCi と同じく IO として読み直す。
        if errs and is_do and "BaseIO" in blob:
            retry = f"#eval show IO _ from{head}\n" + textwrap.indent(src, " " * col)
            again = self.guard(lambda: self.eng.send_cmd(retry))
            if again is not None and panic_check(self.out, again):
                return False
            elif again is not None and not has_error(again):
                render(self.out, again, src, line_off=1, col_off=col)
                self.last = Last(src, advanced=False)
                return True

        # 評価できない式でも、型だけは表示したほうが親切。
        reason = not_evaluable_reason(blob) if errs else None
        if reason is not None:
            out = self.guard(
                lambda: self.eng.query(
                    f"#check{head}\n" + textwrap.indent(src, " " * col)
                )
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
            # エラーになった宣言の sorry は保留しない。sorry を証明で置き換えても
            # 同じエラーで失敗するだけで、置き換えに失敗した直後に「sorry 1 個」
            # と表示してから「sorry 2 個」と表示し直すことになる。
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
        """1 行 = 1 タクティク。proofState を進める。"""
        proof = self.proof
        if proof is None:
            return

        state = proof.state
        resp = self.guard(lambda: self.eng.send_tactic(src, state))
        if self.proof is not proof or proof.gen != self.eng.gen:
            # guard がエンジンを再起動した。leani が持つ proofState は前のプロセスの
            # ものなので、応答があっても別の状態を指している。新しいエンジンは番号を
            # 0 から振り直すので、別の証明の状態と番号が一致して「証明完了」と
            # 表示されることもある (宣言は sorry のまま残る)。
            self.drop_proof()
            return
        elif resp is None or panic_check(self.out, resp):
            return

        self.last = Last(src, advanced=False, proof=True)

        if resp.get("message"):  # エンジンが直接返すエラー
            self.out.fail(red(resp["message"].rstrip()))
            return
        elif has_error(resp):
            render(self.out, resp, src)
            return

        for m in messages(resp):
            if m.get("data"):
                self.out.write(m["data"].rstrip())

        if "proofState" not in resp:
            render(self.out, resp, src)
            return

        found = try_this(messages(resp))
        if found and not balanced(found):
            # 提案を最後まで解析できていない (メッセージの形式が変わった場合など)。
            # 正しくないスクリプトを完成した証明として表示するより、入力したタクティクを
            # そのまま残す。
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
        self.out.write(textwrap.indent(script, "  "))
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
            # revive が replay で sorry を取得し直していれば、そのまま証明モードを
            # 再開できる。
            left = len(self.held.sorries)
            self.out.write(dim(f"  :prove で再開できる (sorry {left} 個)"))
        else:
            # 再起動に失敗したときの proofState は前のプロセスのもの。
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
            # sorry の位置が分からなかった。証明自体は成功しているので、スクリプトは
            # すでに表示してある。何も表示せずに戻ると「証明完了」という表示だけが残る。
            self.out.write(
                dim("-- sorry の位置を読み取れなかったので、宣言は sorry のままにする")
            )
            return

        # sorry を含む宣言と名前が同じになるので、先にそれを取り消してから再実行する。
        undone = None
        if held.env is not None and self.eng.env == held.env:
            undone = self.eng.pop_decl()

        # 再実行に失敗したときに戻せるよう保存しておく。sorry が 2 個以上あるときに
        # 保留中の状態を捨てると、残りの sorry を :prove で続けられなくなる。
        self.held = None

        self.out.write(dim("-- sorry をスクリプトで置き換えて実行する:"))
        self.out.write(textwrap.indent(new_src.strip(), "  "))

        gen = self.eng.gen
        self.submit(new_src, CMD)
        landed = bool(self.eng.log) and self.eng.log[-1] == new_src
        if undone is None or landed:
            # 宣言が環境に追加されたかは log の末尾で判断する。世代だけを見ると、
            # 異常終了したエンジンを guard が再起動して再送し、**成功した**ときにも
            # 「sorry のまま」と誤って表示し、sorry を含むテキストを再実行して、
            # 重複エラーになった宣言がずっと保留に残る。
            return
        elif self.eng.gen != gen:
            # エンジンが再起動され、さらに再実行も失敗した。保存した env id は無効に
            # なっているので設定せず、sorry を含むテキストを再実行する。保留中の状態は
            # 戻さない (replay_into が設定し直した値を無効な proofState で上書きすると、
            # 次の :prove が今の環境に無い状態を指す)。
            self.out.write(dim("-- エンジンを再起動したので、sorry のままにしておく"))
            if undone.src is not None:
                self.replay_into([undone.src])
        else:
            # 再実行が失敗した。項の位置にある sorry にはタクティクを差し込めない。
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

        # 証明モード専用のコマンドを先に確認する。該当しなければ通常のコマンドとして
        # 処理する (:t などは証明中でも使える)。
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
                self.out.write(textwrap.indent("\n".join(proof.script) or "(空)", "  "))
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

        out = self.guard(
            lambda: self.eng.query("#check\n" + textwrap.indent(arg, "  "))
        )
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

        重いパターンだと loogle の処理に 20 秒近くかかる。何も表示しないと leani が
        止まったように見える。成功しても失敗しても、次の出力を表示する前にこの行を消す。
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
        # 入力した宣言も新しいエンジンに引き継ぐ。:l したファイルの内容は preload で
        # 戻るが、対話で入力した宣言は新しい Engine には含まれていない。起動後に
        # 再実行する。
        # 保留中の宣言も引き継ぐ。捨てると、直前に「保留にした」と表示した宣言が
        # :env で何も表示されずに消える。新しい環境ならエラーなく実行
        # できることもある (import が増える方向の切り替え)。失敗したらまた保留に戻る。
        log = list(self.eng.log) + list(self.eng.unplayed)
        self.eng.kill()
        try:
            self._start(target, preload=prev)
        except START_FAILED as e:
            # import が成功するかは boot するまで分からない。元の環境に戻す。
            self.out.fail(
                red(f"{arg} で起動できなかった: {str(e) or '理由は分からない'}")
            )
            self.out.write(dim(f"  {back.name} に戻る"))
            try:
                self._start(back, preload=prev)
            except START_FAILED as back_e:
                # 環境が無いまま続けても、どの入力もエラーになるだけ。端末は
                # 終了し、カーネルは次のセルでエンジンを起動し直す。
                raise EnvLost(f"{back.name} にも戻れなくなった: {back_e}") from back_e

        # 切り替えが成功しても元の環境に戻っても、対話で入力した宣言は新しい
        # エンジンには無い。どちらの場合も、ここで 1 回だけ再実行する。
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

        repl には環境を pickle する機能もあるが、unpickle した環境で #eval すると
        Lean のコンパイラが PANIC する (コンパイラの状態が pickle に含まれない)。
        ソースで保存しておけば編集もできるし、lean でそのまま実行できる。
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
                # 起動時と同じヘッダを書く。設定の import だけだと lean で直接
                # 実行したときにエラーになる (leani は起動時と :l のときに
                # import Lean を自分で追加する)。:l したファイルの import も
                # 追加する (save_header)。
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
            # 環境がすべて置き換わるので、前の環境の proofState は使えない。
            self.proof, self.last, self.undone = None, None, None
            self.clear_pending()
            if announce:
                self.out.write(dim(f"読み込んだ: {path} (env {self.eng.env})"))
            if left:
                # :reset と同じく、何も表示せずに捨てるとユーザーが気付けない。
                self.out.write(dim(f"  保留中の宣言 {left} 件も捨てた"))

        self._comp_cache.clear()

    def reset(self) -> None:
        if self.eng.base is None:
            # 起点の環境が無い (boot に失敗した)。env を設定し直しても、どの入力にも
            # "Unknown environment." しか返さない状態になり、保留中の宣言だけが消える。
            self.out.fail(red("元になる環境が無い。:restart で再起動できる"))
            return

        # 保留も捨てる。残すと、:reset で消したはずの宣言が次の :restart で
        # 戻ってくる。
        left = self.eng.reset()
        self.proof, self.last = None, None
        self.clear_pending()
        self.out.write(dim(f"env {self.eng.env} に戻した"))
        if left:
            self.out.write(dim(f"  保留中の宣言 {left} 件も捨てた"))
