"""leani — Lean 4 の対話 REPL。

エンジンは leanprover-community/repl。あれは JSON in / JSON out の機械向け
プロトコルなので、ここが人間向けの層を持つ。

設計上の要点:

* **完結判定は Lean のパーサに投げる。** 「入力が終わったか」を正規表現で
  近似すると、想定外の構文が来るたびに早く確定しすぎるか次の行を飲み込む。
  `Parser.runParserCategory` を現在の環境で走らせて command / term として
  読めるかを聞く。実行はしない。ユーザ定義の notation も効く。
* **エンジンが落ちても続く。** 受理した宣言のログを持ち、落ちたら再起動して
  replay する。評価中の Ctrl-C も同じ経路で復帰する。
* **セッションはソースで持ち出す。** repl には環境を pickle する機能があるが、
  戻した環境で `#eval` すると Lean のコンパイラが PANIC する。`:save` が
  受理した宣言を `.lean` として書き出し、`:l` でも lean 本体でも読める。
"""

import atexit
import codecs
import hashlib
import json
import os
import re
import select
import shutil
import signal
import subprocess
import sys
import textwrap
import time
import tomllib

HOME = os.path.expanduser("~")
CONFIG_HOME = os.environ.get("XDG_CONFIG_HOME", f"{HOME}/.config")
STATE = os.environ.get("XDG_STATE_HOME", f"{HOME}/.local/state") + "/leani"
CONFIG = os.environ.get("LEANI_CONFIG", f"{CONFIG_HOME}/leani/config.toml")
HIST = os.environ.get("LEANI_HISTORY", f"{STATE}/history")
INIT = os.environ.get("LEANI_INIT", f"{CONFIG_HOME}/leani/init.lean")
# エンジンは leanprover-community/repl の clone。この REPL 本体ではない。
ENGINE = os.environ.get("LEANI_ENGINE", f"{HOME}/sanctum/projects/lean-repl")
PROMPT = "λ> "
# 完結判定と補完のクエリが Lean.Parser / CoreM / Json を使うので、ユーザの
# import が何であれこれだけは要る。import は先頭に並べる決まりなので、
# 常に 1 行目に足しておけば他の import と共存できる。
PROBE_IMPORT = "import Lean\n"

COMPLETE_CAP = 40000

# --------------------------------------------------------------------------- 色

TTY = sys.stdout.isatty()


def c(code, s):
    return f"\033[{code}m{s}\033[0m" if TTY else s


def red(s):
    return c("31", s)


def yellow(s):
    return c("33", s)


def green(s):
    return c("32", s)


def cyan(s):
    return c("36", s)


def dim(s):
    return c("2", s)


# \001 / \002 を解釈するのは readline なので、無いときは埋め込めない。
# libedit は \001..\002 の中身をプロンプトの先頭にまとめて吐くので、
# 色のリセットをプロンプトの末尾に置けない (どちらも _setup_readline で立てる)。
RL_OK = False
RL_HOIST = False


def pc(code, s):
    """プロンプト用の色付け。

    readline (macOS は libedit) はプロンプトの表示幅を数えて折り返し位置と
    カーソル位置を決める。色コードをそのまま置くとそのバイトまで桁として
    数えるので、`\x1b[36mλ> \x1b[0m` は 3 桁なのに 13 桁と見なされ、
    10 桁ずれる。長い行 (履歴から呼び戻した行など) で折り返すと libedit の
    モデルと実際のカーソルが食い違い、Backspace が別のセルを消して文字が
    画面に残る。非表示部分は \001 / \002 で囲んで幅から除く。

    libedit は非表示区間を順番どおりに、ただしすべてプロンプトの先頭へ
    まとめて出す。リセットを末尾に置くと色を出した直後に戻ってしまうので、
    libedit では開きだけを埋め込み、input() を抜けたところで戻す
    (read_line)。入力中の行にも色が乗る。"""
    if not TTY:
        return s
    if not RL_OK:
        return c(code, s)
    open_ = f"\001\033[{code}m\002"
    return open_ + s if RL_HOIST else open_ + s + "\001\033[0m\002"


def reset_sgr():
    if TTY and RL_HOIST:
        sys.stdout.write("\033[0m")
        sys.stdout.flush()


def die(msg):
    print(f"leani: {msg}", file=sys.stderr)
    sys.exit(1)


def lean_str(s):
    """Python の文字列を Lean の文字列リテラルにする。
    JSON のエスケープは Lean の \\n \\t \\\\ \\" \\uXXXX と互換。"""
    return json.dumps(s, ensure_ascii=False)


# ------------------------------------------------------------------ 設定

class ConfigError(Exception):
    """設定が読めない / 指定された環境が無い。"""


class EnvConfig:
    """どの Lake プロジェクトの上で何を import して起動するか。

    ghci の ~/.ghci、ipython の profile と同じ位置づけ。特定のプロジェクト名を
    コードに持たないため、名前は設定ファイル (config.toml) と CLI 引数だけに置く。
    設定が無くても cwd の Lake プロジェクトから推測して動く。
    """

    def __init__(self, name, project=None, imports=(), prompt=None, engine=None):
        self.name = name
        self.project = abspath(project)
        self.imports = list(imports)
        self.prompt = prompt or PROMPT
        self.engine = abspath(engine or ENGINE)

    @property
    def header(self):
        """設定された import 行。:save したファイルの先頭にそのまま書ける形。"""
        return "".join(f"import {m}\n" for m in self.imports)

    @property
    def boot_header(self):
        return PROBE_IMPORT + self.header

    def __str__(self):
        mods = " ".join(["Lean"] + self.imports)
        return f"{self.name} (import {mods})"


def abspath(path):
    return os.path.abspath(os.path.expanduser(path)) if path else None


def load_config(path=CONFIG):
    if not os.path.isfile(path):
        return {}
    try:
        with open(path, "rb") as f:
            return tomllib.load(f)
    except (OSError, tomllib.TOMLDecodeError) as e:
        raise ConfigError(f"設定が読めない: {path}\n  {e}")


def lake_root(start):
    """lakefile を持つ一番近い親ディレクトリ。無ければ None。"""
    d = os.path.abspath(start)
    while True:
        if any(os.path.isfile(f"{d}/{f}")
               for f in ("lakefile.toml", "lakefile.lean")):
            return d
        up = os.path.dirname(d)
        if up == d:
            return None
        d = up


def lake_libs(project):
    """lakefile が公開しているライブラリ名。設定が無いときの import 候補。"""
    toml = f"{project}/lakefile.toml"
    if os.path.isfile(toml):
        try:
            with open(toml, "rb") as f:
                data = tomllib.load(f)
        except (OSError, tomllib.TOMLDecodeError):
            return []
        return [lib["name"] for lib in data.get("lean_lib", []) if "name" in lib]
    try:
        text = open(f"{project}/lakefile.lean").read()
    except OSError:
        return []
    return re.findall(r"^\s*lean_lib\s+«?([A-Za-z0-9_.\']+)»?", text, re.M)


def resolve(name=None, project=None, imports=None, cfg=None):
    """CLI 引数と設定ファイルから、起動する環境を 1 つ決める。

    優先順は 引数 > 名前付き環境 > default > cwd の Lake プロジェクト。
    """
    cfg = load_config() if cfg is None else cfg
    table = cfg.get("env") or {}
    if name is None and not imports and not project:
        name = cfg.get("default")
    entry = {}
    if name is not None:
        if name not in table:
            known = " ".join(sorted(table)) or "(ひとつも無い)"
            raise ConfigError(f"環境 {name} は設定に無い: {CONFIG}\n"
                              f"  ある環境: {known}")
        # 名前で選んだ環境は書いてある通りに使う。cwd は見ない。
        entry = table[name]
        project = project or entry.get("project")
        imports = list(imports or entry.get("imports") or [])
    else:
        if project is None:
            project = lake_root(os.getcwd())
        if project and not imports:
            imports = lake_libs(project)
        name = os.path.basename(project) if project else "plain"
    return EnvConfig(name, project=project, imports=imports,
                     prompt=entry.get("prompt"),
                     engine=entry.get("engine") or cfg.get("engine"))


# ------------------------------------------------------- エンジンに投げるクエリ

# 完結判定。command と term の両方を 1 往復で聞く。
PARSE_PROBE = r"""open Lean Parser in
#eval show CoreM Unit from do
  let src := %s
  let env ← getEnv
  let probe : Name → Json := fun cat =>
    match runParserCategory env cat src with
    | .ok _ => Json.mkObj [("ok", Json.bool true)]
    | .error e => Json.mkObj [("ok", Json.bool false), ("err", Json.str e)]
  IO.println (Json.mkObj [("cmd", probe `command), ("term", probe `term),
                          ("tac", probe `tacticSeq)]).compress"""

COMPLETE_QUERY = r"""open Lean in
#eval show CoreM Unit from do
  let env ← getEnv
  let mut ns : Array String := #[]
  for (n, _) in env.constants.toList do
    if n.isInternalDetail then continue
    let last := n.getString!
    if last.startsWith "_" || last.startsWith "eq_" || last.startsWith "match_"
       || last.startsWith "proof_" || last.startsWith "congr_"
       || last == "go" || last == "loop" || last == "induct" || last == "fun_cases"
       || last == "eq_def" || last == "sizeOf_spec" then continue
    let s := n.toString
    if %s.isPrefixOf s then ns := ns.push s
  IO.println (String.intercalate " " (ns.qsort.toList.take %d))"""

DOC_QUERY = r"""open Lean in
#eval show CoreM Unit from do
  match ← findDocString? (← getEnv) `%s with
  | some d => IO.println d
  | none => pure ()"""

# パーサが「まだ続きがある」と言っているとみなすメッセージ。
INCOMPLETE = re.compile(r"unexpected end of input|unterminated (comment|string)")
# ブロック中でも脱出できるようにする。Lean のソースが行頭 : で始まることは無い
# (`:=` の継続はインデントされる)。
META_LINE = re.compile(r"^:[A-Za-z!?{}]")
# `structure P where` や `induction n with` は Lean 文法ではそれ自体で完結する。
# パーサは「終わり」と言うが、続きのブロックを書きたいのが普通なので確定を遅らせる。
# 完結判定を覆すわけではないので、外しても Enter が 1 回余分に要るだけで済む。
BLOCK_OPEN = re.compile(r"(?:^|[\s)\]}])(where|with|do|by)[ \t]*$")
ERR_POS = re.compile(r"<input>:(\d+):(\d+):")
# 自分で通した宣言の名前。補完に足すためだけなので、取りこぼしても害はない。
DECL_NAME = re.compile(
    r"^\s*(?:@\[[^\]]*\]\s*)*"
    r"(?:private\s+|protected\s+|noncomputable\s+|partial\s+|unsafe\s+|scoped\s+)*"
    r"(?:def|abbrev|theorem|lemma|instance|structure|inductive|class|opaque|axiom)"
    r"\s+([A-Za-z_\u00c0-\uffff][^\s:({\[]*)", re.M)


def err_pos(msg):
    m = ERR_POS.search(msg or "")
    return (int(m.group(1)), int(m.group(2))) if m else (0, 0)


CMD, TERM, TAC = "cmd", "term", "tac"             # 送り方
COMPLETE, MORE, ERR = "complete", "more", "err"   # 入力の状態


def indented(line):
    return line[:1] in (" ", "\t")


def continues(line):
    """この行が単独の入力になりえないか。インデント行と、行頭の `|`。
    `|` で始まる command も tactic も Lean には無いので、必ず前の行の続き。"""
    return indented(line) or line.lstrip().startswith("|")


# --------------------------------------------------------------------- エンジン


class EngineDied(Exception):
    pass


class Interrupted(Exception):
    pass


class Engine:
    """repl サブプロセス 1 個。落ちたら restart() で作り直す。"""

    def __init__(self, cfg):
        self.cfg = cfg
        self.tc = self._toolchain()
        self.lean_path = self._lean_path()
        self.proc = None
        self.env = None          # いまの環境 id
        self.base = None         # 起動直後 / :l 直後の環境 id
        self.stack = []          # :undo 用
        self.log = []            # 受理した宣言。再起動時に replay する
        self.loaded = None       # :l したファイル
        self.spawn()

    # -- 環境変数 ---------------------------------------------------------

    def _toolchain(self):
        """lean は cwd の lean-toolchain を見て版を決める。プロジェクトの外から
        呼ぶと既定の版が選ばれて olean が読めなくなるので、elan run で固定する。"""
        try:
            engine_tc = open(f"{self.cfg.engine}/lean-toolchain").read().strip()
        except OSError as e:
            die(f"エンジンの lean-toolchain が読めない: {e}")
        tc = engine_tc
        if self.cfg.project:
            try:
                tc = open(f"{self.cfg.project}/lean-toolchain").read().strip()
            except OSError:
                pass
        if tc != engine_tc:
            print(
                yellow(
                    f"警告: toolchain が違う "
                    f"(プロジェクト={tc} / エンジン={engine_tc})。\n"
                    f"  cd {self.cfg.engine} && git pull && lake build repl"
                ),
                file=sys.stderr,
            )
        return tc

    def _lean_path(self):
        env = dict(os.environ, **self._lake_env())
        env["LEAN_PATH"] = (f"{self.cfg.engine}/.lake/build/lib/lean:"
                            + env.get("LEAN_PATH", ""))
        return env

    def _lake_env(self):
        """lake env が解決する環境変数。

        lake env は起動に 1 秒近くかかるので、lake-manifest.json より新しい
        キャッシュがあれば使い回す。"""
        if not self.cfg.project:
            return {}
        key = hashlib.sha1(self.cfg.project.encode()).hexdigest()[:12]
        cache = f"{STATE}/lake-env/{os.path.basename(self.cfg.project)}-{key}"
        manifest = f"{self.cfg.project}/lake-manifest.json"
        stale = not os.path.isfile(cache) or (
            os.path.isfile(manifest)
            and os.path.getmtime(manifest) > os.path.getmtime(cache))
        if stale:
            script = "\n".join(
                f'printf "{k}=%s\\n" "${{{k}-}}"'
                for k in ("LEAN_PATH", "LEAN_SRC_PATH",
                          "DYLD_LIBRARY_PATH", "LD_LIBRARY_PATH"))
            r = subprocess.run(["lake", "env", "sh", "-c", script],
                               cwd=self.cfg.project, capture_output=True, text=True)
            if r.returncode != 0:
                die(f"lake env が失敗した ({self.cfg.project}):\n{r.stderr.strip()}")
            os.makedirs(os.path.dirname(cache), exist_ok=True)
            with open(f"{cache}.tmp", "w") as f:
                f.write(r.stdout)
            os.replace(f"{cache}.tmp", cache)
        out = {}
        try:
            for line in open(cache):
                if "=" in line:
                    k, v = line.rstrip("\n").split("=", 1)
                    out[k] = v
        except OSError:
            pass
        return out

    # -- プロセス ---------------------------------------------------------

    def spawn(self):
        self.proc = subprocess.Popen(
            ["elan", "run", self.tc, f"{self.cfg.engine}/.lake/build/bin/repl"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=None,
            env=self.lean_path, text=True, bufsize=1,
            start_new_session=True,      # Ctrl-C を自分のプロセス群だけに向ける
        )

    def kill(self):
        if self.proc and self.proc.poll() is None:
            try:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                self.proc.kill()

    # -- プロトコル -------------------------------------------------------

    def _send(self, obj):
        """リクエストを 1 つ投げて、レスポンス 1 つを読む。

        読み取りは生の fd と select でやる。バッファ付きの readline では
        Ctrl-C がどこで効いたのか (リクエストが飛んだのか、レスポンスを
        取りこぼしたのか) が分からず、プロトコルがずれる恐れがある。
        ここで Interrupted を上げたら呼び出し側は必ずエンジンを作り直す。"""
        try:
            return self._exchange(obj)
        except KeyboardInterrupt:
            raise Interrupted()

    def _exchange(self, obj):
        if self.proc.poll() is not None:
            raise EngineDied()
        try:
            self.proc.stdin.write(json.dumps(obj) + "\n\n")
            self.proc.stdin.flush()
        except (BrokenPipeError, ValueError):
            raise EngineDied()
        fd = self.proc.stdout.fileno()
        dec = codecs.getincrementaldecoder("utf-8")(errors="replace")
        buf = ""
        while True:
            try:
                ready, _, _ = select.select([fd], [], [], 0.25)
            except (OSError, ValueError):
                raise EngineDied()
            if not ready:
                if self.proc.poll() is not None:
                    raise EngineDied()
                continue
            try:
                chunk = os.read(fd, 1 << 16)
            except OSError:
                raise EngineDied()
            if not chunk:
                raise EngineDied()
            buf += dec.decode(chunk)
            # レスポンスは空行で終わる。JSON の中に空行が来ることもあるので、
            # 空行の候補を順に試して最初に読めたものを採る。
            for m in re.finditer(r"\n[ \t]*\n", buf):
                head = buf[:m.start()]
                if not head.strip():
                    continue
                try:
                    return json.loads(head)
                except json.JSONDecodeError:
                    continue

    def send_cmd(self, src, fresh=False):
        req = {"cmd": src}
        if not fresh and self.env is not None:
            req["env"] = self.env
        return self._send(req)

    def query(self, src):
        """info メッセージの中身だけ取る。環境は進めない。"""
        resp = self.send_cmd(src)
        if any(m.get("severity") == "error" for m in resp.get("messages", [])):
            return None
        return "\n".join(
            m.get("data", "") for m in resp.get("messages", [])
            if m.get("severity") == "info"
        )

    def advance(self, resp):
        if self.env is not None:
            self.stack.append(self.env)
        self.env = resp["env"]

    # -- 起動 / 再起動 ----------------------------------------------------

    def boot(self):
        """設定された import を流して起点の環境を作る。

        pickle キャッシュは試したが効かないので入れていない。repl の pickle は
        import からの差分しか持たない (1.2KB 程度) ので、unpickle でも
        olean の読み込みは同じだけ走る。実測でも import 1.3s / unpickle 1.2s、
        mathlib は 5.4s / 5.3s で差が無い。さらに戻した環境で #eval すると
        Lean のコンパイラが PANIC する。セッションの保存は :save (ソース) で行う。"""
        self.env, self.stack = None, []
        resp = self.send_cmd(self.cfg.boot_header.rstrip("\n"), fresh=True)
        if any(m.get("severity") == "error" for m in resp.get("messages", [])):
            raise EngineDied()
        self.env = self.base = resp["env"]

    def restart(self):
        """落ちた / 中断されたエンジンを作り直し、宣言を replay する。"""
        self.kill()
        self.spawn()
        log, loaded = list(self.log), self.loaded
        self.log = []
        self.boot()
        if loaded and os.path.isfile(loaded):
            try:
                self.load_file(loaded)
            except OSError:
                pass        # 読めなくなっていても replay は続ける
        for src in log:
            try:
                resp = self.send_cmd(src)
                if not any(m.get("severity") == "error"
                           for m in resp.get("messages", [])):
                    self.advance(resp)
                    self.log.append(src)
            except (EngineDied, Interrupted):
                break
        return len(self.log)

    def load_file(self, path, src=None):
        if src is None:
            with open(path) as f:
                src = f.read()
        if not re.search(r"^\s*import\s", src, re.M):
            src = self.cfg.header + src
        src = PROBE_IMPORT + src
        keep = (self.env, self.stack, self.log)
        self.env, self.stack, self.log = None, [], []
        resp = self.send_cmd(src, fresh=True)
        bad = any(m.get("severity") == "error" for m in resp.get("messages", []))
        if not bad:
            self.env = self.base = resp["env"]
            self.loaded = path
        else:
            # 失敗した読み込みで手元の環境まで失わない。env が None のままだと
            # 以後の入力が import 無しの環境に飛んで、何を書いても通らなくなる。
            self.env, self.stack, self.log = keep
        return resp, bad, src


# ------------------------------------------------------------------ 表示

def panic_check(resp):
    """エンジンが PANIC を吐いたら黙って結果扱いしない。"""
    for m in resp.get("messages", []):
        if "PANIC at" in (m.get("data") or ""):
            print(red("エンジンが PANIC した。:restart で作り直すのが安全。"))
            print(dim((m["data"].splitlines() or [""])[0]))
            return True
    return False


# `exact?` や `simp?` は結果を "Try this:" として info で返す。台本にはこの
# 中身を入れる。`exact?` のままでは :save したファイルで毎回検索が走り、
# 結果も環境次第で変わる。
SUGGESTION = re.compile(r"^\s*(?:\[[^\]]*\]\s*)?(\S.*?)\s*$")


def try_this(messages):
    """"Try this:" の提案のうち最初のものを返す。無ければ None。"""
    for m in messages or []:
        data = (m.get("data") or "").strip()
        if not data.startswith("Try this:"):
            continue
        for line in data.splitlines()[1:]:
            hit = SUGGESTION.match(line)
            if hit and hit.group(1):
                return hit.group(1)
    return None


def render(resp, src, line_off=0, col_off=0):
    """メッセージを GHCi 風に出す。位置があれば該当行とキャレットを添える。"""
    lines = src.splitlines()
    for m in resp.get("messages", []):
        sev = m.get("severity", "information")
        paint = {"error": red, "warning": yellow}.get(sev, lambda x: x)
        pos = m.get("pos") or {}
        ln = pos.get("line", 0) - line_off
        col = pos.get("column", 0) - col_off
        data = (m.get("data") or "").rstrip()
        if sev in ("error", "warning") and 1 <= ln <= len(lines):
            src_line = lines[ln - 1]
            col = max(0, min(col, len(src_line)))
            end = m.get("endPos") or {}
            width = 1
            if end.get("line", 0) - line_off == ln:
                width = max(1, end.get("column", 0) - col_off - col)
            width = min(width, max(1, len(src_line) - col))
            head = f"{ln}:{col + 1}"
            print(dim(head) + "  " + src_line)
            print(" " * (len(head) + 2 + col) + paint("^" * width))
            print(paint(textwrap.indent(data, "  ")))
        else:
            print(paint(data))
    for i, sy in enumerate(resp.get("sorries", [])):
        print(green(f"sorry {i + 1} [proofState {sy.get('proofState')}]"))
        print(textwrap.indent((sy.get("goal") or "").rstrip(), "  "))


# ------------------------------------------------------------------ フロント

HELP = """\
式を書くと #eval される。宣言はそのまま通る。入力が終わったかは Lean のパーサが決める。
続きがある行はそのまま次の行を待ち、インデントを続ける限り読む。空行で確定。
確定した直後にインデント行を書けば、直前の入力に遡って続きとして読み直す。

  :t <expr>      型 (#check)
  :i <name>      型と docstring
  :p <name>      定義 (#print)
  :l <file>      読み込む (環境を作り直す)   :r  読み直す
  :reset         起動直後に戻る              :undo [n]  n 個前の環境へ
  :env [name]    今の環境 / 設定した環境に切り替えて再起動
  :prove [n]     sorry の証明モードに入る   :goals  残っている sorry と目標
  :save <file>   通した宣言を .lean に書き出す (:l で読み直せる)
  :time          実行時間の表示を切り替え
  :{ ... :}      複数行を明示的に囲む
  :! <cmd>       shell
  :restart       エンジンを作り直して宣言を replay
  :help :?       これ                        :q  終了 (Ctrl-D)

証明モード (⊢>) では 1 行が 1 タクティク。:goals :script :undo :done
"""

PROOF_HELP = "証明モード: 1 行 = 1 タクティク。:goals 目標  :script 台本  :undo 戻す  :done 出る"


class Repl:
    _rl_ready = False

    def __init__(self, cfg, preload=None):
        self.cfg = cfg
        self.buf = []
        self.ready = None      # ブロックが構文的に完結しているときの送り方
        self.last = None       # 直前に送った入力 (インデント行で遡るため)
        self.explicit = False
        self.show_time = False
        self.proof = None
        self.pending = []
        self.proof_src = None       # sorry を出した宣言のソース
        self.sorry_env = None       # その宣言が作った環境 id (照合用)
        self._comp_cache = {}
        self._hits = []
        self._own = (-1, [])
        self._hist_added = 0
        self._last_submitted = None
        self.eng = Engine(cfg)
        self._setup_readline()
        t0 = time.time()
        self.eng.boot()
        print(dim(f"leani — {self.eng.tc} / {cfg}"
                  f" / {time.time() - t0:.1f}s"))
        if os.path.isfile(INIT):
            self.apply_init()
        if preload:
            self.load(preload)

    def apply_init(self):
        """init ファイルを起動直後の環境に重ねる。

        :l と違って環境を作り直さない。ここで通したものは base に含めるので
        :reset しても残る (GHCi の .ghci と同じ扱い)。"""
        try:
            with open(INIT) as f:
                lines = f.read().splitlines()
        except OSError as e:
            return print(red(f"{INIT} が読めない ({e.strerror})"))
        src = "\n".join(l for l in lines
                         if not l.lstrip().startswith("import "))
        if not src.strip():
            return
        resp = self.guard(lambda: self.eng.send_cmd(src))
        if resp is None:
            return
        if any(m.get("severity") == "error" for m in resp.get("messages", [])):
            print(red(f"{INIT} にエラーがある:"))
            render(resp, src)
            return
        self.eng.env = self.eng.base = resp["env"]
        print(dim(f"-- {INIT} を読んだ"))

    # -- readline ---------------------------------------------------------

    def _setup_readline(self):
        try:
            import readline
        except ImportError:
            self.rl = None
            return
        self.rl = readline
        global RL_OK, RL_HOIST
        RL_OK = True
        RL_HOIST = "libedit" in (readline.__doc__ or "")
        readline.set_completer(self._complete)
        if Repl._rl_ready:
            return
        Repl._rl_ready = True
        os.makedirs(os.path.dirname(HIST), exist_ok=True)
        try:
            readline.read_history_file(HIST)
        except OSError:
            pass
        readline.set_history_length(10000)
        atexit.register(self._save_history)
        # Lean の名前は . を含むので区切りにしない。
        readline.set_completer_delims(" \t\n(),[]{};\"")
        if RL_HOIST:
            readline.parse_and_bind("bind ^I rl_complete")
        else:
            readline.parse_and_bind("tab: complete")

    def _save_history(self):
        """毎行書く。atexit だけだと落ちたセッションの履歴が丸ごと消える。
        1 万件でもミリ秒なので、書き直しのコストは問題にならない。"""
        if not self.rl:
            return
        try:
            self.rl.write_history_file(HIST)
        except OSError:
            pass

    def _merge_history(self, src):
        """複数行のブロックを履歴 1 件にまとめる。

        readline は行単位なので `def fib` の 4 行は 4 件になり、呼び戻すのに
        Ctrl-P が 4 回要る。末尾の n 件がそのブロックそのものだと確認できた
        ときだけ 1 件に置き換える (数え違いで履歴を壊さないため)。
        libedit は履歴ファイル上で改行を \012 として往復できる。"""
        n, self._hist_added = self._hist_added, 0
        if not self.rl or n < 2 or src is None:
            return
        total = self.rl.get_current_history_length()
        if total < n:
            return
        items = [self.rl.get_history_item(i) for i in range(total - n + 1, total + 1)]
        if any(x is None for x in items):
            return
        if "\n".join(items).rstrip() != src.rstrip():
            return
        for _ in range(n):
            self.rl.remove_history_item(self.rl.get_current_history_length() - 1)
        self.rl.add_history(src)
        self._save_history()      # まとめた形をファイルにも反映する

    def _complete(self, text, state):
        if state == 0:
            self._hits = self._names(text)
        hits = self._hits
        return hits[state] if state < len(hits) else None

    @staticmethod
    def _chunk(prefix):
        """まとめて取る単位。名前空間があればそこまで、無ければ先頭 2 文字。

        mathlib では 1 文字だと `C` で 7.5 万件 (3.4MB) になるので広げすぎない。
        `Nat.` なら 5684 件、`MeasureTheory.` なら 1 万件で収まる。"""
        return prefix[:prefix.rfind(".") + 1] if "." in prefix else prefix[:2]

    def _names(self, prefix):
        """定数名の prefix 検索。名前空間ごとの塊を 1 度だけ取って以後は絞る。"""
        if len(prefix) < 2:
            return []
        # 塊は base 環境 (import / :l 直後) に紐付ける。宣言を 1 つ通すたびに
        # 捨てていると mathlib では毎回 1.1 秒かかり直すので、自分で通した分だけ
        # Python 側で足す。
        key = (self.eng.base, self._chunk(prefix))
        chunk = self._comp_cache.get(key)
        if chunk is None:
            out = self.guard(
                lambda: self.eng.query(
                    COMPLETE_QUERY % (lean_str(key[1]), COMPLETE_CAP))
            )
            if out is None:
                return []
            chunk = out.split()
            self._comp_cache[key] = chunk
        hits = {x for x in chunk if x.startswith(prefix)}
        hits.update(x for x in self._own_names() if x.startswith(prefix))
        return sorted(hits)

    def _own_names(self):
        """REPL で通した宣言の名前。ログが伸びたときだけ数え直す。"""
        if self._own[0] != len(self.eng.log):
            names = set()
            for src in self.eng.log:
                names.update(DECL_NAME.findall(src))
            self._own = (len(self.eng.log), sorted(names))
        return self._own[1]

    # -- エンジンの面倒を見る ---------------------------------------------

    def guard(self, fn, on_dead=None):
        """エンジンが落ちる / 中断されたら再起動して replay する。

        落ちた場合は作り直したうえで 1 回だけやり直す。ユーザが Ctrl-C で
        止めた場合はやり直さない (止めたいのだから)。"""
        retry = False
        try:
            return fn()
        except Interrupted:
            print(yellow("^C 中断した。エンジンを作り直す…"))
        except EngineDied:
            print(red("エンジンが落ちた。作り直す…"))
            retry = True
        n = self.eng.restart()
        self.proof, self.last = None, None
        self.clear_pending()
        print(dim(f"宣言 {n} 件を replay した (env {self.eng.env})"))
        if retry:
            try:
                return fn()
            except (Interrupted, EngineDied):
                pass
        return on_dead

    # -- 完結判定 ---------------------------------------------------------

    def parse(self, src):
        """Lean のパーサに command / term / tacticSeq として読めるかを聞く。
        1 往復で 3 つとも取る。実行はしないので、ユーザ定義の notation も効く。"""
        out = self.guard(lambda: self.eng.query(PARSE_PROBE % lean_str(src)))
        if out:
            try:
                return json.loads(out.strip().splitlines()[-1])
            except (json.JSONDecodeError, IndexError):
                pass
        return None

    def probe(self, src):
        """(state, kind) を返す。state は complete / more / err。
        判定できなければ err にして、そのまま投げて Lean に本当のエラーを出させる
        (自前の推測でエラーを作らない)。"""
        r = self.parse(src)
        if r is None:
            return ERR, CMD
        cmd, term = r.get("cmd", {}), r.get("term", {})
        if cmd.get("ok"):
            return COMPLETE, CMD
        if term.get("ok"):
            return COMPLETE, TERM
        ce, te = cmd.get("err", ""), term.get("err", "")
        # 深く進めた側が、ユーザの意図した種類。
        kind = CMD if err_pos(ce) >= err_pos(te) else TERM
        if INCOMPLETE.search(ce) or INCOMPLETE.search(te):
            return MORE, kind
        return ERR, kind

    def probe_tac(self, src):
        r = self.parse(src)
        if r is None:
            return ERR, TAC
        tac = r.get("tac", {})
        if tac.get("ok"):
            return COMPLETE, TAC
        if INCOMPLETE.search(tac.get("err", "")):
            return MORE, TAC
        return ERR, TAC

    # -- ループ -----------------------------------------------------------

    def prompt(self):
        if self.proof is not None:
            return pc("35", "⊢> ")
        return pc("36", self.cfg.prompt)

    def read_line(self, prompt):
        try:
            return input(prompt)
        finally:
            reset_sgr()

    def loop(self):
        while True:
            try:
                line = self.read_line(pc("2", " | ")
                                      if (self.buf or self.explicit)
                                      else self.prompt())
            except EOFError:
                if self.buf or self.explicit:
                    self.buf, self.ready, self.explicit = [], None, False
                    self._hist_added = 0
                    print()
                    continue
                print()
                return 0
            except KeyboardInterrupt:
                self.buf, self.ready, self.explicit = [], None, False
                self.last = None
                self._hist_added = 0
                print("^C")
                continue
            if line.strip():
                self._hist_added += 1
            self._save_history()
            # まとめた履歴を呼び戻すと改行入りの 1 行として返ってくるので、
            # 打ったときと同じ順に食わせ直す。
            lines = line.split("\n")
            for i, one in enumerate(lines):
                if i:
                    self._hist_added += 1
                if self.feed(one) == "quit":
                    return 0
            if len(lines) > 1 and (self.buf or self.explicit):
                if self.feed("") == "quit":      # 空行で確定させる
                    return 0

    def feed(self, line):
        """1 行受け取る。

        完結したかはパーサに聞くが、それだけでは足りない。Lean では
        `structure P where` や `def f := 1` はそれ自体で完結した command なので、
        パーサは「終わり」と言う。にもかかわらず次のインデント行は続きになりうる。
        そこで二段構えにする:

        * パーサが「途中」と言えば継続行を読む (ブロックに入る)。
        * ブロック中は、インデント行が続く限り読む。空行かインデントの切れた行で確定。
        * ブロックに入らず確定したあとにインデント行が来たら、直前の入力に
          遡って続きとして読み直す (環境も 1 つ戻す)。
        """
        if self.explicit:
            if line.strip() == ":}":
                src, self.buf, self.explicit = "\n".join(self.buf), [], False
                if src.strip():
                    self.submit(src)
            else:
                self.buf.append(line)
            return

        if not self.buf:
            s = line.strip()
            if not s:
                self.last = None
                return
            if s == ":{":
                self.explicit, self.buf = True, []
                return
            if s.startswith(":"):
                self.last = None
                self._hist_added = 0
                return self.meta(s)
            if continues(line) and self.last is not None:
                self.rewind()
                self.buf = self.last["src"].splitlines() + [line]
                self.last = None
            else:
                self.buf = [line]
        else:
            if not line.strip():                       # 空行で確定
                src, kind = "\n".join(self.buf).rstrip(), self.ready
                self.buf, self.ready = [], None
                if src.strip():
                    self.submit(src, kind)
                return
            if META_LINE.match(line):           # ブロックからの脱出
                src, kind = "\n".join(self.buf), self.ready
                self.buf, self.ready = [], None
                if kind is not None:
                    self.submit(src, kind)
                else:
                    print(dim("-- 未完のまま破棄した"))
                    self.last = None
                return self.feed(line)
            if self.ready is not None and not continues(line):
                # 確定済みのブロックにインデントの切れた行 → ここで切って読み直す
                src, kind = "\n".join(self.buf), self.ready
                self.buf, self.ready = [], None
                self.submit(src, kind)
                return self.feed(line)
            self.buf.append(line)

        src = "\n".join(self.buf)
        state, kind = self.probe_tac(src) if self.proof is not None else self.probe(src)
        if state == MORE:
            self.ready = None
            return
        if (len(self.buf) > 1 and continues(self.buf[-1])) or BLOCK_OPEN.search(src):
            self.ready = kind         # 完結。ただし続きがありうるので空行を待つ
            return
        self.buf, self.ready = [], None
        self.submit(src, kind)

    def rewind(self):
        """直前の入力を取り消して、続きを書けるようにする。"""
        if not self.last.get("advanced"):
            return
        if self.last.get("proof") and self.proof is not None:
            if self.proof["stack"]:
                self.proof["state"] = self.proof["stack"].pop()
            if self.proof["script"]:
                self.proof["script"].pop()
            return
        if self.eng.stack:
            self.eng.env = self.eng.stack.pop()
        if self.eng.log:
            self.eng.log.pop()

    # -- 送信 -------------------------------------------------------------

    def submit(self, src, kind=None):
        self._last_submitted = src
        self._merge_history(src)
        if self.proof is not None:
            return self.tactic(src)
        if kind is None:
            _, kind = self.probe(src)
        t0 = time.time()
        self.last = None
        if kind == TERM:
            wrapped = "#eval\n" + textwrap.indent(src, "  ")
            resp = self.guard(lambda: self.eng.send_cmd(wrapped))
            if resp is None:
                return
            if panic_check(resp):
                return
            errs = [m for m in resp.get("messages", []) if m.get("severity") == "error"]
            blob = "\n".join(m.get("data", "") for m in errs)
            # `do` を単体で書くと最初の action からモナドが決まってしまう
            # (IO.getEnv なら BaseIO)。GHCi と同じく IO と読み直してやる。
            if errs and src.lstrip().startswith("do") and "BaseIO" in blob:
                retry = "#eval show IO _ from\n" + textwrap.indent(src, "  ")
                r2 = self.guard(lambda: self.eng.send_cmd(retry))
                if r2 is not None and not any(
                    m.get("severity") == "error" for m in r2.get("messages", [])
                ):
                    render(r2, src, line_off=1, col_off=2)
                    self.last = {"src": src, "advanced": False}
                    self.timing(t0)
                    return
            if errs and re.search(
                r"noncomputable|failed to compile|could not synthesize.*"
                r"(Repr|ToString|ToExpr|Eval)|cannot evaluate", blob, re.S | re.I
            ):
                out = self.guard(
                    lambda: self.eng.query("#check\n" + textwrap.indent(src, "  "))
                )
                if out:
                    print(out.rstrip())
                    print(dim("-- 評価できないので型だけ"))
                    self.last = {"src": src, "advanced": False}
                    self.timing(t0)
                    return
            render(resp, src, line_off=1, col_off=2)
            self.last = {"src": src, "advanced": False}
            self.timing(t0)
            return

        env_before = self.eng.env
        resp = self.guard(lambda: self.eng.send_cmd(src))
        if resp is None:
            return
        if panic_check(resp):
            return
        bad = any(m.get("severity") == "error" for m in resp.get("messages", []))
        advanced = not bad and "env" in resp
        if advanced:
            self.eng.advance(resp)
            self.eng.log.append(src)
        render(resp, src)
        sorries = resp.get("sorries") or []
        self.clear_pending()
        self.pending = sorries
        if sorries:
            # env id を覚えておく。:undo などで環境が動いたら埋め戻しでは
            # 巻き戻さない (二重に pop して手前の宣言を落とすため)。
            self.proof_src = src
            self.sorry_env = self.eng.env if advanced else None
            print(dim(f"-- :prove で証明モードに入る (sorry {len(sorries)} 個)"))
        self.last = {"src": src, "advanced": advanced, "env_before": env_before}
        self.timing(t0)

    def clear_pending(self):
        """sorry まわりの持ち越しを捨てる。環境が動いたら proofState は無効。"""
        self.pending = []
        self.proof_src, self.sorry_env = None, None

    def timing(self, t0):
        if self.show_time:
            print(dim(f"({time.time() - t0:.2f}s)"))

    # -- 証明モード -------------------------------------------------------

    def tactic(self, src):
        st = self.proof
        before = st["state"]
        resp = self.guard(
            lambda: self.eng._send({"tactic": src, "proofState": before})
        )
        if resp is None:
            return
        self.last = {"src": src, "advanced": False, "proof": True}
        if resp.get("message"):                    # エンジンからの素のエラー
            print(red(resp["message"].rstrip()))
            return
        if any(m.get("severity") == "error" for m in resp.get("messages", [])):
            render(resp, src)
            return
        for m in resp.get("messages", []):
            if m.get("data"):
                print(m["data"].rstrip())
        if "proofState" not in resp:
            render(resp, src)
            return
        st["stack"].append(before)
        st["state"] = resp["proofState"]
        found = try_this(resp.get("messages"))
        if found and found != src:
            print(dim(f"-- 台本には {found} を入れた"))
        st["script"].append(found or src)
        self.last = {"src": src, "advanced": True, "proof": True}
        goals = resp.get("goals", [])
        if not goals:
            script = "\n".join(st["script"])
            print(green("証明完了。"))
            print(dim("-- 台本:"))
            print(textwrap.indent(script, "  "))
            self.proof, self.last = None, None
            self.close_sorry(script)
            return
        self.show_goals(goals)

    def close_sorry(self, script):
        """`by sorry` を台本で埋め戻して、宣言を本物として通し直す。"""
        src = self.proof_src
        if not src or src.count("sorry") != 1:
            return
        new_src = re.sub(r"[ \t]*sorry", "\n" + textwrap.indent(script, "  "),
                         src, count=1)
        if self.sorry_env is not None and self.eng.env == self.sorry_env:
            if self.eng.stack:
                self.eng.env = self.eng.stack.pop()
            if self.eng.log:
                self.eng.log.pop()
        self.proof_src, self.sorry_env = None, None
        print(dim("-- 埋め戻して通す:"))
        print(textwrap.indent(new_src.strip(), "  "))
        self.submit(new_src, CMD)

    def show_goals(self, goals=None):
        if goals is None:
            goals = self.proof["goals"] if self.proof else []
        if self.proof is not None:
            self.proof["goals"] = goals
        for i, g in enumerate(goals):
            head = f"goal {i + 1}/{len(goals)}" if len(goals) > 1 else "goal"
            print(green(head))
            print(textwrap.indent(g, "  "))

    # -- メタコマンド -----------------------------------------------------

    def meta(self, line):
        if line.startswith(":!"):
            subprocess.run(line[2:].strip(), shell=True)
            return
        parts = line.split(None, 1)
        cmd, arg = parts[0][1:], (parts[1].strip() if len(parts) > 1 else "")

        if self.proof is not None:
            if cmd in ("goals", "g"):
                return self.show_goals()
            if cmd == "script":
                print(textwrap.indent("\n".join(self.proof["script"]) or "(空)", "  "))
                return
            if cmd == "undo":
                if self.proof["stack"]:
                    self.proof["state"] = self.proof["stack"].pop()
                    if self.proof["script"]:
                        self.proof["script"].pop()
                    print(dim(f"proofState {self.proof['state']}"))
                else:
                    print(dim("戻る先が無い"))
                return
            if cmd in ("done", "q", "quit"):
                self.proof, self.buf, self.ready = None, [], None
                print(dim("証明モードを出た"))
                return
            if cmd in ("?", "h", "help"):
                print(PROOF_HELP)
                return

        if cmd in ("q", "quit"):
            return "quit"
        if cmd in ("?", "h", "help"):
            print(HELP, end="")
            return
        # 証明が通ると自動で抜けるので、その直後に打たれても困らせない。
        if cmd in ("goals", "g"):
            if self.pending:
                for i, sy in enumerate(self.pending):
                    print(green(f"sorry {i + 1} [proofState {sy.get('proofState')}]"))
                    print(textwrap.indent((sy.get("goal") or "").rstrip(), "  "))
            else:
                print(dim("証明モードではない (sorry も残っていない)"))
            return
        if cmd in ("script", "done"):
            print(dim("証明モードではない"))
            return
        if cmd in ("t", "type"):
            if not arg:
                return print(red(":t には式が要る"))
            out = self.guard(lambda: self.eng.query("#check\n" + textwrap.indent(arg, "  ")))
            print(out.rstrip() if out else red("型が取れなかった"))
            return
        if cmd in ("i", "info"):
            if not arg:
                return print(red(":i には名前が要る"))
            out = self.guard(lambda: self.eng.query(f"#check @{arg}"))
            print(out.rstrip() if out else red(f"不明: {arg}"))
            doc = self.guard(lambda: self.eng.query(DOC_QUERY % arg))
            if doc and doc.strip():
                print(dim(doc.strip()))
            return
        if cmd in ("p", "print"):
            if not arg:
                return print(red(":p には名前が要る"))
            resp = self.guard(lambda: self.eng.send_cmd(f"#print {arg}"))
            if resp:
                render(resp, arg)
            return
        if cmd in ("l", "load"):
            return self.load(arg)
        if cmd in ("r", "reload"):
            return self.load(self.eng.loaded) if self.eng.loaded else self.reset()
        if cmd == "reset":
            return self.reset()
        if cmd == "undo":
            n = int(arg) if arg.isdigit() else 1
            for _ in range(n):
                if self.eng.stack:
                    self.eng.env = self.eng.stack.pop()
                    if self.eng.log:
                        self.eng.log.pop()
            self.clear_pending()
            print(dim(f"env {self.eng.env}"))
            return
        if cmd == "env":
            return self.cmd_env(arg)
        if cmd == "prove":
            return self.prove(arg)
        if cmd == "save":
            return self.cmd_save(arg)
        if cmd == "time":
            self.show_time = not self.show_time
            print(dim(f"実行時間の表示: {'on' if self.show_time else 'off'}"))
            return
        if cmd == "restart":
            n = self.eng.restart()
            self.clear_pending()
            print(dim(f"作り直した。宣言 {n} 件を replay (env {self.eng.env})"))
            return
        print(red(f"不明なコマンド: :{cmd}  (:help)"))

    def cmd_env(self, arg):
        """引数なしで今の環境、名前を渡すとその環境で起動し直す。"""
        if not arg:
            print(dim(f"{self.cfg} — {self.cfg.project or 'Lake プロジェクト無し'}"))
            print(dim(f"env {self.eng.env} "
                      f"(base {self.eng.base}, 宣言 {len(self.eng.log)} 件)"))
            try:
                names = sorted(load_config().get("env") or {})
            except ConfigError as e:
                return print(red(str(e)))
            if names:
                print(dim(f"切り替え先: {' '.join(names)}  (:env <name>)"))
            return
        if arg == self.cfg.name:
            return print(dim(f"すでに {arg}"))
        try:
            target = resolve(name=arg)
        except ConfigError as e:
            return print(red(str(e)))
        keep, prev = self.show_time, self.eng.loaded
        self.eng.kill()
        self.__init__(target, preload=prev)
        self.show_time = keep

    def prove(self, arg):
        sorries = self.pending
        if not sorries:
            return print(red("直前の入力に sorry が無い"))
        i = int(arg) - 1 if arg.isdigit() else 0
        if not (0 <= i < len(sorries)):
            return print(red(f"sorry は {len(sorries)} 個。1..{len(sorries)} で指定する"))
        s = sorries[i]
        self.proof = {"state": s["proofState"], "goals": [s.get("goal", "")],
                      "script": [], "stack": []}
        print(dim(PROOF_HELP))
        self.show_goals([s.get("goal", "")])

    def cmd_save(self, arg):
        """通した宣言を .lean として書き出す。

        repl には環境を pickle する機能もあるが、戻した環境で #eval すると
        Lean のコンパイラが PANIC する (コンパイラの状態が pickle に入らない)。
        ソースで持っておけば編集もできるし lean でそのまま走る。"""
        if not arg:
            return print(red(":save にはファイル名が要る"))
        if not self.eng.log:
            return print(red("保存する宣言が無い"))
        path = os.path.abspath(os.path.expanduser(arg))
        body = "\n\n".join(src.rstrip() for src in self.eng.log)
        with open(path, "w") as f:
            f.write(f"{self.cfg.header}\n{body}\n")
        print(dim(f"宣言 {len(self.eng.log)} 件を書き出した: {path}"))
        print(dim(f"-- :l {path}"))

    def load(self, path, announce=True):
        if not path:
            return print(red("ファイル名が要る"))
        path = os.path.expanduser(path)
        if not os.path.isfile(path):
            return print(red(f"ファイルが無い: {path}"))
        try:
            with open(path) as f:
                src = f.read()
        except OSError as e:
            return print(red(f"読めない: {path} ({e.strerror})"))
        out = self.guard(lambda: self.eng.load_file(path, src))
        if out is None:
            return
        resp, bad, src = out
        render(resp, src)
        if bad:
            print(red(f"読み込めなかった: {path}"))
        elif announce:
            print(dim(f"読み込んだ: {path} (env {self.eng.env})"))
        self._comp_cache.clear()

    def reset(self):
        self.eng.env, self.eng.stack, self.eng.log = self.eng.base, [], []
        self.proof, self.last = None, None
        self.clear_pending()
        print(dim(f"env {self.eng.env} に戻した"))


def main(argv=None):
    args = list(sys.argv if argv is None else argv)[1:]
    name, project, imports, preload, version = None, None, [], None, False
    while args:
        a = args.pop(0)
        if a in ("-e", "--env"):
            name = args.pop(0) if args else die(f"{a} には名前が要る")
        elif a in ("-i", "--import"):
            imports.append(args.pop(0) if args else die(f"{a} にはモジュール名が要る"))
        elif a in ("-p", "--project"):
            project = args.pop(0) if args else die(f"{a} にはディレクトリが要る")
        elif a in ("-h", "--help"):
            print(HELP, end="")
            return 0
        elif a in ("-V", "--version"):
            version = True
        elif a.startswith("-") and a != "-":
            die(f"不明なオプション: {a}  (-h で使い方)")
        else:
            preload = a
    try:
        cfg = resolve(name, project, imports)
    except ConfigError as e:
        return die(str(e))
    if version:
        print("leani")
        print(f"  環境:     {cfg}")
        print(f"  プロジェクト: {cfg.project or '(無し)'}")
        print(f"  エンジン: {cfg.engine}")
        print(f"  設定:     {CONFIG}"
              f"{'' if os.path.isfile(CONFIG) else ' (無し)'}")
        print(f"  init:     {INIT}")
        print(f"  履歴:     {HIST}")
        return 0
    if cfg.project and not os.path.isdir(cfg.project):
        die(f"Lake プロジェクトが無い: {cfg.project}")
    if not os.path.isdir(cfg.engine):
        die(f"repl エンジンが無い: {cfg.engine}\n"
            f"  git clone https://github.com/leanprover-community/repl {cfg.engine}")
    if not os.path.isfile(f"{cfg.engine}/.lake/build/bin/repl"):
        die(f"repl が未ビルド: cd {cfg.engine} && lake build repl")
    if not shutil.which("elan"):
        die("elan が PATH に無い")
    return Repl(cfg, preload).loop()


if __name__ == "__main__":
    sys.exit(main())
