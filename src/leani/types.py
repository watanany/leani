"""型と例外 (純粋)。

repl とやりとりする JSON の形、送り方のラベル、それに例外。"""

from __future__ import annotations

from typing import Any, Final, Literal, TypedDict, TypeVar

# こちらが組む JSON と、TOML の設定。形がその場ごとに違うので dict のまま扱う。
Json = dict[str, Any]

# repl から返る JSON。実行時はそのままの dict で扱い、欠けた鍵は `.get` で流す
# (repl のバージョンが変わっても落ちないように)。TypedDict にしてあるのは鍵の名前と値の
# 型を mypy に見てもらうためで、検証はしない。どの鍵も「返ってくるかもしれない」
# ものなので total=False にする。
#
# 見てもらえる範囲は 2 つ。添字 (`resp["env"]`) の名前と、取り出した値の型。
# `.get("間違った名前")` は Mapping.get として通ってしまうが、返りが object になる
# ので使った所で落ちる。


class Pos(TypedDict, total=False):
    """ソースの位置。line は 1 から、column は 0 から数える。"""

    line: int
    column: int


class Message(TypedDict, total=False):
    """エラー・警告・info の 1 件。"""

    severity: str  # "error" / "warning" / "information"
    data: str
    pos: Pos
    endPos: Pos


class Sorry(TypedDict, total=False):
    """`sorry` 1 個。proofState は repl プロセスの中の番号 (作り直すと振り直す)。"""

    proofState: int
    goal: str
    pos: Pos
    endPos: Pos


class Response(TypedDict, total=False):
    """repl の応答ひとつ。"""

    env: int
    proofState: int
    messages: list[Message]
    sorries: list[Sorry]
    goals: list[str]
    message: str  # エンジンが直接返すエラー。messages ではなくこちらに入る


class Parse(TypedDict, total=False):
    """1 つの構文カテゴリとして読めたか (PARSE_PROBE の応答の中身)。"""

    ok: bool
    err: str


class Probe(TypedDict, total=False):
    """PARSE_PROBE の応答。command / term / tacticSeq を 1 往復で聞いた結果。"""

    cmd: Parse
    term: Parse
    tac: Parse


# loogle の応答。こちらは repl ではなく外部サービスへの HTTP で、エラーも 200 で返る
# (`error` の鍵があるかどうかで見分ける)。同じく検証はしない。


class Hit(TypedDict, total=False):
    """loogle が挙げた宣言 1 件。type は先頭に空白が付いた形で返る。"""

    name: str
    type: str
    module: str
    doc: str | None


class Loogle(TypedDict, total=False):
    """loogle の応答。見つかったときは hits、駄目だったときは error が入る。"""

    count: int
    header: str  # 「Found N declarations ...」の 2 行
    hits: list[Hit]
    error: str
    suggestions: list[str]  # 名前が違うときの候補


Kind = Literal["cmd", "term", "tac"]  # 送り方
State = Literal["complete", "more", "err"]  # 入力の状態
Step = Literal["probe", "done", "quit"]  # 1 行処理したあと何をするか

CMD: Final[Kind] = "cmd"
TERM: Final[Kind] = "term"
TAC: Final[Kind] = "tac"
COMPLETE: Final[State] = "complete"
MORE: Final[State] = "more"
ERR: Final[State] = "err"

T = TypeVar("T")


# 例外は投げるモジュールと受けるモジュールが違うので、どちらにも寄せずここに置く。状態を
# 持たないので、純粋な側から投げても副作用を呼ぶことにはならない。
class ConfigError(Exception):
    """設定が読めない / 指定された環境が無い。"""


class EngineError(Exception):
    """エンジンを用意できない。起動を断る理由になるが、REPL は落とさない。"""


class EngineDied(Exception):
    """repl プロセスが応答しなくなった。呼び出し側は作り直す。"""


class Interrupted(Exception):
    """評価中に Ctrl-C が来た。プロトコルがずれているので作り直す。"""


class NoEnvironment(Exception):
    """
    環境が無いのに環境の上で走らせようとした。呼ぶ側の誤り。

    boot が通らなかったあとの状態を扱い忘れると、以前は repl が Init だけの
    環境を勝手に作って答えていた (嘘の型、消える宣言)。黙って進むよりは
    ここで止める。loop が「内部エラー」として 1 行分に留めるので、セッション
    ごと落ちることはない。
    """


class SearchError(Exception):
    """外部サービスに届かなかった。ネットワークの事情なので、報告だけで済ませる。"""


# 起動が駄目になる理由。どれも報告して済ませる (traceback にしない)。
START_FAILED = (EngineError, EngineDied, Interrupted, OSError)
