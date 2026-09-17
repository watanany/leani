"""型と例外 (純粋)。

repl とやりとりする JSON の別名、送り方の札、それに例外。"""

from __future__ import annotations

from typing import Any, Final, Literal, TypeVar

# repl とやりとりする JSON。キーは repl 側の都合で決まるので dict のまま扱い、
# 別名で意味だけ持たせる。
Json = dict[str, Any]
Message = Json  # {"severity": ..., "data": ..., "pos": ..., "endPos": ...}
Sorry = Json  # {"proofState": ..., "goal": ...}
Response = Json  # repl の返事ひとつ

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


# 起動が駄目になる理由。どれも報告して済ませる (traceback にしない)。
START_FAILED = (EngineError, EngineDied, Interrupted, OSError)
