"""型と例外 (純粋)。

repl とやりとりする JSON の形、送り方のラベル、それに例外。"""

from __future__ import annotations

from typing import Any, Final, Literal, TypedDict, TypeVar

# leani が組み立てる JSON と、TOML の設定。形が場所ごとに違うので dict のまま扱う。
Json = dict[str, Any]

# repl から返る JSON。実行時はそのままの dict で扱い、無いキーは `.get` で読み飛ばす
# (repl のバージョンが変わっても leani が異常終了しないように)。TypedDict にしてあるのは
# キーの名前と値の型を mypy に検査させるためで、実行時の検証はしない。どのキーも
# 「返ってくるかもしれない」ものなので total=False にする。
#
# mypy が検査できる範囲は 2 つ。添字 (`resp["env"]`) の名前と、取り出した値の型。
# `.get("間違った名前")` は Mapping.get として型チェックでエラーにならないが、戻り値の
# 型が object になるので、その値を使った所で型エラーになる。


class Pos(TypedDict, total=False):
    """ソースの位置。line は 1 から、column は 0 から数える。"""

    line: int
    column: int


class Message(TypedDict, total=False):
    """エラー、警告、info のどれか 1 件。"""

    severity: str  # "error" / "warning" / "information"
    data: str
    pos: Pos
    endPos: Pos


class Sorry(TypedDict, total=False):
    """`sorry` 1 個。proofState は repl プロセスの中の番号 (再起動すると変わる)。"""

    proofState: int
    goal: str
    pos: Pos
    endPos: Pos


class Response(TypedDict, total=False):
    """repl の応答 1 つ。"""

    env: int
    proofState: int
    messages: list[Message]
    sorries: list[Sorry]
    goals: list[str]
    message: str  # エンジンが直接返すエラー。messages ではなくこのキーに含まれる


class Parse(TypedDict, total=False):
    """1 つの構文カテゴリとして読めたか (PARSE_PROBE の応答の中身)。"""

    ok: bool
    err: str


class Probe(TypedDict, total=False):
    """PARSE_PROBE の応答。command / term / tacticSeq を 1 回で確かめた結果。"""

    cmd: Parse
    term: Parse
    tac: Parse


class Scope(TypedDict, total=False):
    """SCOPE_QUERY の応答。短い名前がどの名前空間の名前か。"""

    # (名前空間, hiding で隠した名前)。namespace の中なら、その名前空間と親の
    # 名前空間も含まれる。
    open: list[tuple[str, list[str]]]
    # `open X (a)` や renaming で追加した (短い名前, 完全修飾名)。
    alias: list[tuple[str, str]]


# loogle の応答。repl ではなく外部サービスへの HTTP リクエストの応答で、エラーも
# ステータス 200 で返る (`error` キーがあるかどうかで区別する)。こちらも実行時の
# 検証はしない。


class Hit(TypedDict, total=False):
    """loogle が返した宣言 1 件。type は先頭に空白が付いた形で返る。"""

    name: str
    type: str
    module: str
    doc: str | None


class Loogle(TypedDict, total=False):
    """loogle の応答。見つかったときは hits、失敗したときは error が含まれる。"""

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


# 例外は raise するモジュールと except するモジュールが違うので、どちらでもなく
# ここに置く。例外は状態を持たないので、純粋なモジュールから raise しても副作用には
# ならない。
class ConfigError(Exception):
    """設定が読めない / 指定された環境が無い。"""


class EngineError(Exception):
    """エンジンを用意できない。起動時なら起動を中止する。起動後なら REPL は続ける。"""


class EngineDied(Exception):
    """repl プロセスが応答しなくなった。呼び出し側は repl を再起動する。"""


class Interrupted(Exception):
    """
    評価中に Ctrl-C が押された。

    repl とのやりとりがずれているので、呼び出し側は repl を再起動する。
    """


class NoEnvironment(Exception):
    """
    環境が無いのに、環境の上で入力を実行しようとした。呼び出し側のバグ。

    環境を指定せずに送ると、repl は Init だけの環境を作って答える (型が正しくない、
    宣言が消える)。エラーを出さずに
    進むよりは、ここで止める。loop はこの例外を「内部エラー」として 1 行分の失敗に
    とどめるので、セッション全体が異常終了することはない。
    """


class SearchError(Exception):
    """外部サービスへの問い合わせが失敗した。ネットワークの問題なので、報告するだけにする。"""


# 起動が失敗する理由。どれも報告するだけにする (traceback は出さない)。
START_FAILED = (EngineError, EngineDied, Interrupted, OSError)
