"""パス (読み取り)。

設定、履歴、キャッシュのパスと、環境変数で決まる値。起動時に一度読むだけで、
それ以降は定数として扱う。"""

from __future__ import annotations

import os
import sys

HOME = os.path.expanduser("~")
CONFIG_HOME = os.environ.get("XDG_CONFIG_HOME", f"{HOME}/.config")
STATE = os.environ.get("XDG_STATE_HOME", f"{HOME}/.local/state") + "/leani"
CONFIG = os.environ.get("LEANI_CONFIG", f"{CONFIG_HOME}/leani/config.toml")
HIST = os.environ.get("LEANI_HISTORY", f"{STATE}/history")
INIT = os.environ.get("LEANI_INIT", f"{CONFIG_HOME}/leani/init.lean")
# エンジンは leanprover-community/repl で、leani 自体ではない。エンジンが無ければ
# leani が clone してビルドする (ensure_engine)。使う Lean のバージョンごとに
# ディレクトリを作る。
ENGINE_REPO = "https://github.com/leanprover-community/repl"
ENGINE_CACHE = f"{STATE}/engine"
# LEANI_ENGINE が指定されたときは、leani は何も管理せず、そのディレクトリを
# そのまま使う。
ENGINE = os.environ.get("LEANI_ENGINE")
NO_SETUP = bool(os.environ.get("LEANI_NO_SETUP"))
# エンジンを用意するときの、外部との通信のタイムアウト (秒)。応答が返らないまま
# 何も表示せずに止まらないように。
SETUP_TIMEOUT = 120
# git に認証情報を入力させない。capture_output だと、入力を求めるプロンプトが
# 表示されないまま止まる。
SETUP_ENV = dict(os.environ, GIT_TERMINAL_PROMPT="0")
PROMPT = "λ> "
# 完結判定と補完のクエリが Lean.Parser / CoreM / Json を使うので、ユーザーの
# import が何であっても、この import は必要。import はファイルの先頭に並べる
# 決まりなので、常に 1 行目に追加しておけば他の import と一緒に使える。
PROBE_IMPORT = "import Lean\n"

# 起動時のヘッダが本当に読み込まれたかを確かめるための 1 行。repl は解決できない
# import があると、**エラーも出さずに**ヘッダ全体を捨てて新しい env を返す。なので、
# この行がエラーなく実行できるかでしか判別できない。そのとき import Lean も捨て
# られるため、確かめる対象は Lean の中の定数にする (完結判定が使っているものと同じ)。
BOOT_PROBE = "#check @Lean.Parser.runParserCategory\n"

COMPLETE_CAP = 40000

# 定理検索 (loogle)。エンジンとは別のサービスで、外部に HTTP で問い合わせる。自分で
# 用意した loogle も指定できる。公開されている loogle は Mathlib を検索するので、
# 手元に無い名前も結果に含まれる (module を一緒に表示するのはそのため)。
LOOGLE = os.environ.get("LEANI_LOOGLE", "https://loogle.lean-lang.org/json")
# 応答が返らないときもプロンプトが戻るように。重いパターン (部分項をたくさん
# 書いたもの) は loogle 側で heartbeats の上限に達するまで実行されるので、実測で
# 20 秒近くかかる。短くすると、結果が返るはずの検索まで打ち切ってしまう。
LOOGLE_TIMEOUT = 30

# 色を付けるかどうかは起動時に決める。パイプに出力するときは色を付けない。
TTY = sys.stdout.isatty()
