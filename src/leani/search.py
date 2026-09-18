"""外に聞く (副作用)。

loogle に定理を聞く。エンジンとは無関係で、返るのは JSON だけ。整形は pure に
ある。ここが唯一の外向きの口で、失敗はすべて SearchError にまとめる (網の事情で
REPL を落とさない)。"""

from __future__ import annotations

import json
import urllib.error
import urllib.parse
import urllib.request
from typing import cast

from leani.places import LOOGLE, LOOGLE_TIMEOUT
from leani.types import Loogle, SearchError


def loogle(query: str) -> Loogle:
    """loogle に 1 回聞く。届かなければ SearchError。"""
    url = f"{LOOGLE}?q={urllib.parse.quote(query)}"
    try:
        with urllib.request.urlopen(url, timeout=LOOGLE_TIMEOUT) as res:
            got = json.load(res)
    except urllib.error.HTTPError as e:
        raise SearchError(f"{e.code} {e.reason}") from e
    except (OSError, json.JSONDecodeError) as e:
        raise SearchError(str(e)) from e

    if not isinstance(got, dict):
        raise SearchError("返事が JSON の表ではない")

    return cast(Loogle, got)
