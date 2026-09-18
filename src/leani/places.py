"""パス (読み取り)。

設定・履歴・キャッシュの位置と、環境変数で決まる値。起動時に一度読むだけで、
ここから先は定数として扱う。"""

from __future__ import annotations

import os
import sys

HOME = os.path.expanduser("~")
CONFIG_HOME = os.environ.get("XDG_CONFIG_HOME", f"{HOME}/.config")
STATE = os.environ.get("XDG_STATE_HOME", f"{HOME}/.local/state") + "/leani"
CONFIG = os.environ.get("LEANI_CONFIG", f"{CONFIG_HOME}/leani/config.toml")
HIST = os.environ.get("LEANI_HISTORY", f"{STATE}/history")
INIT = os.environ.get("LEANI_INIT", f"{CONFIG_HOME}/leani/init.lean")
# エンジンは leanprover-community/repl。この REPL 本体ではない。無ければ leani が
# 取ってきてビルドする (ensure_engine)。使う Lean のバージョンごとに掘る。
ENGINE_REPO = "https://github.com/leanprover-community/repl"
ENGINE_CACHE = f"{STATE}/engine"
# 明示されたときは leani は何も管理せず、そのディレクトリをそのまま使う。
ENGINE = os.environ.get("LEANI_ENGINE")
NO_SETUP = bool(os.environ.get("LEANI_NO_SETUP"))
# エンジンの用意で外部と通信するときの上限。応答が返らないまま黙って止まらないように。
SETUP_TIMEOUT = 120
# git に認証を聞き返させない。capture_output だと聞かれても見えないまま止まる。
SETUP_ENV = dict(os.environ, GIT_TERMINAL_PROMPT="0")
PROMPT = "λ> "
# 完結判定と補完のクエリが Lean.Parser / CoreM / Json を使うので、ユーザの
# import が何であれこれだけは必要。import は先頭に並べる決まりなので、
# 常に 1 行目に足しておけば他の import と共存できる。
PROBE_IMPORT = "import Lean\n"

# 起動したヘッダが本当に効いたかを見るための 1 行。repl は解決できない import
# を**エラーも出さずに**ヘッダごと捨てて新しい env を返すので、これが通るかで
# しか判別できない。import Lean まで落ちるため、指すのはその中の定数にする
# (完結判定が使っているものそのまま)。
BOOT_PROBE = "#check @Lean.Parser.runParserCategory\n"

COMPLETE_CAP = 40000

# 定理検索 (loogle)。エンジンとは別のサービスで、こちらは外部への HTTP。自分で
# 用意したものを指せるようにしてある。公開のものが探す先は mathlib なので、手元に
# 無い名前も出てくる (module を一緒に出すのはそのため)。
LOOGLE = os.environ.get("LEANI_LOOGLE", "https://loogle.lean-lang.org/json")
# 応答が返らないときもプロンプトが戻るように。重いパターン (部分項をたくさん
# 書いたもの) は向こうの heartbeats 上限に達するまで走るので、実測で 20 秒近く
# かかる。短くすると、答えが出るはずのものまで打ち切ってしまう。
LOOGLE_TIMEOUT = 30

# 色を出すかどうかは起動時に決まる。パイプに出力するときは混ぜない。
TTY = sys.stdout.isatty()
