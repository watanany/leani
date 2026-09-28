"""外部サービスに問い合わせる (副作用)。

loogle で定理を検索する。エンジンとは関係なく、loogle は JSON だけを返す。整形は
pure にある。外部と通信するのはこのモジュールだけで、失敗はすべて SearchError に
まとめる (ネットワークの問題で REPL を終了させないため)。"""

from __future__ import annotations

import json
import urllib.error
import urllib.parse
import urllib.request
from typing import cast

from leani.places import LOOGLE, LOOGLE_TIMEOUT
from leani.types import Loogle, SearchError


def loogle(query: str) -> Loogle:
    """loogle に 1 回問い合わせる。失敗したら SearchError を raise する。"""
    url = f"{LOOGLE}?q={urllib.parse.quote(query)}"
    try:
        with urllib.request.urlopen(url, timeout=LOOGLE_TIMEOUT) as res:
            got = json.load(res)
    except urllib.error.HTTPError as e:
        raise SearchError(f"{e.code} {e.reason}") from e
    except (OSError, json.JSONDecodeError) as e:
        raise SearchError(str(e)) from e

    if not isinstance(got, dict):
        raise SearchError("応答が JSON のオブジェクトではない")

    return cast(Loogle, got)
