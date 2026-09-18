"""型と例外 (純粋)。

repl とやりとりする JSON の形、送り方の札、それに例外。"""

from __future__ import annotations

from typing import Any, Final, Literal, TypedDict, TypeVar

# こちらが組む JSON と、TOML の設定。形がその場ごとに違うので dict のまま扱う。
Json = dict[str, Any]

# repl から返る JSON。実行時は素の dict のままで、欠けた鍵は `.get` で流す
# (repl の版が変わっても落ちないように)。TypedDict にしてあるのは鍵の綴りと値の
# 型を mypy に見てもらうためで、検証はしない。どの鍵も「返ってくるかもしれない」
# ものなので total=False にする。
#
# 見てもらえる範囲は 2 つ。添字 (`resp["env"]`) の綴りと、取り出した値の型。
# `.get("綴り間違い")` は Mapping.get として通ってしまうが、返りが object になる
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
    """repl の返事ひとつ。"""

    env: int
    proofState: int
    messages: list[Message]
    sorries: list[Sorry]
    goals: list[str]
    message: str  # エンジンからの素のエラー。messages ではなくこちらで来る


class Parse(TypedDict, total=False):
    """1 つの構文カテゴリとして読めたか (PARSE_PROBE の返事の中身)。"""

    ok: bool
    err: str


class Probe(TypedDict, total=False):
    """PARSE_PROBE の返事。command / term / tacticSeq を 1 往復で聞いた結果。"""

    cmd: Parse
    term: Parse
    tac: Parse


# loogle の返り。こちらは repl ではなく外の HTTP で、エラーも 200 で来る
# (`error` の鍵があるかどうかで見分ける)。同じく検証はしない。


class Hit(TypedDict, total=False):
    """loogle が挙げた宣言 1 件。type は先頭に空白が付いて来る。"""

    name: str
    type: str
    module: str
    doc: str | None


class Loogle(TypedDict, total=False):
    """loogle の返事。当たったときは hits、駄目なときは error が入る。"""

    count: int
    header: str  # 「Found N declarations ...」の 2 行
    hits: list[Hit]
    error: str
    suggestions: list[str]  # 綴り違いのときの候補


Kind = Literal["cmd", "term", "tac"]  # 送り方
State = Literal["complete", "more", "err"]  # 入力の状態
Step = Literal["probe", "done", "quit"]  # 1 行食べたあと何をするか

CMD: Final[Kind] = "cmd"
TERM: Final[Kind] = "term"
TAC: Final[Kind] = "tac"
COMPLETE: Final[State] = "complete"
MORE: Final[State] = "more"
ERR: Final[State] = "err"

T = TypeVar("T")


# 例外は投げる節と受ける節が違うので、どちらにも寄せずここに置く。状態を
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
    """外の検索に届かなかった。網の事情なので、REPL は落とさず報告して済ませる。"""


# 起動が駄目になる理由。どれも報告して済ませる (traceback にしない)。
START_FAILED = (EngineError, EngineDied, Interrupted, OSError)
