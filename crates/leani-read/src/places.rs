//! パスと、環境変数で決まる値 (読み取り)。
//!
//! 環境変数、カレントディレクトリ、PATH は `main` が起動時に 1 回だけ読んで `Ctx` を
//! 作る。ほかの関数は `Ctx` を引数で受け取り、環境変数を直接読まない。テストは
//! 同じプロセスの中で並列に動くので、プロセス全体の状態を書き換えると、ほかのテストの
//! 結果が変わってしまうためである。

use std::io::IsTerminal;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub const PROMPT: &str = "λ> ";

// エンジンは leanprover-community/repl で、leani 自体ではない。エンジンが無ければ
// leani が clone してビルドする (ensure_engine)。使う Lean のバージョンごとに
// ディレクトリを作る。
pub const ENGINE_REPO: &str = "https://github.com/leanprover-community/repl";

// エンジンを用意するときの、すぐに終わるはずのコマンド (`lean --version` と
// `git ls-remote`) のタイムアウト。応答が返らないまま何も表示せずに止まらない
// ように。clone、ビルド、`lake env` は時間を区切らず、Ctrl-C で止める。
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(120);

// 完結判定と補完のクエリが Lean.Parser / CoreM / Json を使うので、ユーザーの
// import が何であっても、この import は必要。import はファイルの先頭に並べる
// 決まりなので、常に 1 行目に追加しておけば他の import と一緒に使える。
pub const PROBE_IMPORT: &str = "import Lean\n";

// 起動時のヘッダが本当に読み込まれたかを確かめるための 1 行。repl は解決できない
// import があると、**エラーも出さずに**ヘッダ全体を捨てて新しい env を返す。なので、
// この行がエラーなく実行できるかでしか判別できない。そのとき import Lean も捨て
// られるため、確かめる対象は Lean の中の定数にする (完結判定が使っているものと同じ)。
pub const BOOT_PROBE: &str = "#check @Lean.Parser.runParserCategory\n";

pub const COMPLETE_CAP: usize = 40000;

// 定理検索 (loogle)。エンジンとは別のサービスで、外部に HTTP で問い合わせる。自分で
// 用意した loogle も指定できる。公開されている loogle は Mathlib を検索するので、
// 今の環境に無い名前も結果に含まれる (module を一緒に表示するのはそのため)。
pub const LOOGLE: &str = "https://loogle.lean-lang.org/json";

// 応答が返らないときもプロンプトが戻るように。重いパターン (部分項をたくさん
// 書いたもの) は loogle 側で heartbeats の上限に達するまで実行されるので、実測で
// 20 秒近くかかる。短くすると、結果が返るはずの検索まで打ち切ってしまう。
pub const LOOGLE_TIMEOUT: Duration = Duration::from_secs(30);

/// 起動時に決まる値。作ったあとは変えない (中断のフラグだけは別)。
#[derive(Clone, Debug)]
pub struct Ctx {
    pub home: String,
    pub cwd: String,
    /// コマンドを探すときと、子プロセスに渡すときに使う PATH。
    pub path: String,
    /// 起動時の LEAN_PATH。Lake プロジェクトが無いときは、エンジンの .olean の場所を
    /// この先頭に足して repl に渡す。
    pub lean_path: Option<String>,
    pub config: String,
    pub hist: String,
    pub init: String,
    /// leani が書き込むファイルの置き場所 (`XDG_STATE_HOME/leani`)。
    pub state: String,
    pub engine_cache: String,
    /// LEANI_ENGINE が指定されたときは、leani は何も管理せず、そのディレクトリを
    /// そのまま使う。
    pub engine: Option<String>,
    pub no_setup: bool,
    /// エンジンを用意するときの外部コマンドのタイムアウト。テストでは短くする。
    pub setup_timeout: Duration,
    pub loogle: String,
    /// `:abbrev` の一覧を表示する pager (`MANPAGER`、`PAGER`、無ければ `less`)。
    pub pager: String,
    pub tty: bool,
    /// 色を付けるかどうかは起動時に決める。パイプに出力するときは色を付けない。
    /// Jupyter のカーネルは stdout が端末ではないが、ノートブックは ANSI の色を
    /// 表示できるので、kernelspec で FORCE_COLOR を指定して色を付ける。
    pub color: bool,
    /// Ctrl-C で立つ。SIGINT のハンドラは bin だけが登録する。確かめた箇所で
    /// `swap(false)` で読んで消す。
    pub interrupted: Arc<AtomicBool>,
}

impl Ctx {
    /// 環境変数を var で読んで作る。
    ///
    /// Python 版の `environ.get(k, default)` と同じく、空文字列の環境変数は「設定済み」と
    /// 見なす。LEANI_NO_SETUP と FORCE_COLOR だけは、空文字列なら設定していないと見なす。
    pub fn new(var: impl Fn(&str) -> Option<String>, cwd: &str, tty: bool) -> Ctx {
        let home = var("HOME").unwrap_or_default();
        let config_home = var("XDG_CONFIG_HOME").unwrap_or_else(|| format!("{home}/.config"));
        let state = var("XDG_STATE_HOME").unwrap_or_else(|| format!("{home}/.local/state"));
        let state = format!("{state}/leani");
        let set = |k: &str| var(k).is_some_and(|v| !v.is_empty());
        Ctx {
            cwd: cwd.to_string(),
            path: var("PATH").unwrap_or_else(|| "/bin:/usr/bin".to_string()),
            lean_path: var("LEAN_PATH"),
            config: var("LEANI_CONFIG")
                .unwrap_or_else(|| format!("{config_home}/leani/config.toml")),
            hist: var("LEANI_HISTORY").unwrap_or_else(|| format!("{state}/history")),
            init: var("LEANI_INIT").unwrap_or_else(|| format!("{config_home}/leani/init.lean")),
            engine_cache: format!("{state}/engine"),
            state,
            engine: var("LEANI_ENGINE"),
            no_setup: set("LEANI_NO_SETUP"),
            setup_timeout: SETUP_TIMEOUT,
            loogle: var("LEANI_LOOGLE").unwrap_or_else(|| LOOGLE.to_string()),
            pager: var("MANPAGER")
                .or_else(|| var("PAGER"))
                .unwrap_or_else(|| "less".to_string()),
            tty,
            color: tty || set("FORCE_COLOR"),
            interrupted: Arc::new(AtomicBool::new(false)),
            home,
        }
    }

    /// このプロセスの環境変数、カレントディレクトリ、stdout から作る。`main` だけが呼ぶ。
    pub fn from_process() -> Ctx {
        let var = |k: &str| match (std::env::var(k).ok(), k) {
            (None, "HOME") => std::env::home_dir().map(|p| p.to_string_lossy().into_owned()),
            (v, _) => v,
        };
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "/".to_string());
        Ctx::new(var, &cwd, std::io::stdout().is_terminal())
    }
}
