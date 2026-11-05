"""パス (読み取り)。

設定、履歴、キャッシュのパスと、環境変数で決まる値。起動時に 1 回読むだけで、
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
ENGINE_REPO = "https://github.com/leanprover-community/repl"
ENGINE_CACHE = f"{STATE}/engine"
ENGINE = os.environ.get("LEANI_ENGINE")
NO_SETUP = bool(os.environ.get("LEANI_NO_SETUP"))
# エンジンを用意するときの、`git ls-remote` と `lake env` のタイムアウト (秒)。
# 応答が返らないまま何も表示せずに止まらないように。`lean --version` (elan が
# toolchain を取得することがある) は時間を区切らず、Ctrl-C で止める。
SETUP_TIMEOUT = 120
# git に認証情報を入力させない。capture_output だと、入力を求めるプロンプトが
# 表示されないまま止まる。
SETUP_ENV = dict(os.environ, GIT_TERMINAL_PROMPT="0")
PROMPT = "λ> "
PROBE_IMPORT = "import Lean\n"

BOOT_PROBE = "#check @Lean.Parser.runParserCategory\n"

COMPLETE_CAP = 40000

LOOGLE = os.environ.get("LEANI_LOOGLE", "https://loogle.lean-lang.org/json")
LOOGLE_TIMEOUT = 30

TTY = sys.stdout.isatty()
COLOR = TTY or bool(os.environ.get("FORCE_COLOR"))
