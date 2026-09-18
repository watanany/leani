"""外部サービスに問い合わせる (副作用)。

loogle に定理を問い合わせる。エンジンとは無関係で、返るのは JSON だけ。整形は
pure にある。外部と通信するのはこのモジュールだけで、失敗はすべて SearchError に
まとめる (ネットワークの事情で REPL を終了させない)。"""

from __future__ import annotations

import json
import urllib.error
import urllib.parse
import urllib.request
from typing import cast

from leani.places import LOOGLE, LOOGLE_TIMEOUT
from leani.types import Loogle, SearchError


def loogle(query: str) -> Loogle:
    """loogle に 1 回問い合わせる。届かなければ SearchError。"""
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
