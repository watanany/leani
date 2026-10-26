//! エンジンを使うテストの共通部分 (Python 版の `conftest.py` に当たる)。
//!
//! `spec` は `crates/leani/tests/*.rs` だけを読むので、このファイルのテストは
//! SPEC に載らない。ここにはテストを書かない。
//!
//! どのテストも Lean 本体だけを import する環境で実行する。特定の Lake プロジェクト
//! に依存しないので、このリポジトリの外に持ち出してもそのまま実行できる。

#![allow(dead_code)]

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, LazyLock, Mutex};

use leani::engine::{Hooked, Stage};
use leani::repl::Repl;
use leani::search::Search;
use leani_core::pure::text;
use leani_core::types::{Json, Output, SearchError};
use leani_read::config::{load_config, resolve};
use leani_read::places::Ctx;
use regex::Regex;

/// 設定、履歴、init を置く一時ディレクトリ。`Ctx` と同じ寿命で持つ。
pub struct Sandbox {
    pub ctx: Ctx,
    pub dir: tempfile::TempDir,
}

/// 環境変数は PATH と HOME などプロセスのものを使い、設定、履歴、init だけを
/// 一時ディレクトリに向ける。環境は core (既定) と wide の 2 つ。
///
/// Python 版は環境変数そのものを書き換えていたが、Rust 版の `Ctx` は値を持つだけ
/// なので、テストごとに別の `Ctx` を作れる。
pub fn sandbox() -> Sandbox {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    fs::write(
        &config,
        "default = \"core\"\n\n[env.core]\nimports = []\n\n[env.wide]\nimports = [\"Lean.Elab.Frontend\"]\nprompt = \"λ+> \"\n",
    )
    .expect("write config");
    let at = |p: &Path| p.to_string_lossy().into_owned();
    let vars = [
        ("LEANI_INIT", at(&dir.path().join("no-such-init.lean"))),
        ("LEANI_HISTORY", at(&dir.path().join("history"))),
        ("LEANI_CONFIG", at(&config)),
    ];
    let var = |k: &str| match vars.iter().find(|(name, _)| *name == k) {
        Some((_, v)) => Some(v.clone()),
        None if k.starts_with("LEANI_") => None,
        None => std::env::var(k).ok(),
    };
    let cwd = std::env::current_dir().expect("cwd");
    let ctx = Ctx::new(var, &at(&cwd), false);
    Sandbox { ctx, dir }
}

// ------------------------------------------------------------------ 表示の受け取り

/// 表示を文字列にためるだけの `Output`。`Driver` が中身を読んで返す。
#[derive(Clone, Default)]
pub struct Sink(pub Arc<Mutex<String>>);

impl Output for Sink {
    fn put(&mut self, text: &str) {
        self.0.lock().expect("sink").push_str(text);
    }

    fn fail(&mut self, text: &str) {
        self.write(text);
    }

    fn shell(&mut self, _cmd: &str) {}

    fn page(&mut self, text: &str) {
        self.write(text);
    }
}

// ------------------------------------------------------------------ 偽の loogle

/// 決めた応答を返す loogle (Python 版の `mocker.patch.object(leani.repl, "loogle")`)。
/// 応答を決めていなければ、問い合わせが無かったことにして失敗を返す。
#[derive(Clone, Default)]
pub struct FakeSearch {
    answer: Arc<Mutex<Option<Result<Json, String>>>>,
    calls: Arc<AtomicUsize>,
}

impl Search for FakeSearch {
    fn loogle(&self, _query: &str) -> Result<Json, SearchError> {
        self.calls.fetch_add(1, SeqCst);
        match self.answer.lock().expect("answer").clone() {
            Some(Ok(got)) => Ok(got),
            Some(Err(e)) => Err(SearchError(e)),
            None => Err(SearchError("応答を決めていない".to_string())),
        }
    }
}

static ANSI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*[A-Za-z]|\x1b[()][A-Za-z0-9]").expect("regex"));

/// 色とカーソルの制御を取り除く。
pub fn strip_ansi(s: &str) -> String {
    ANSI.replace_all(s, "").into_owned()
}

// ------------------------------------------------------------------ 不変条件

/// 1 行渡すごとに確かめる。1・2 番目は `Engine` のメソッドだけが変える。`Repl` は
/// `Engine` の状態を書き換えない。
/// 不変条件 1 つ。成り立たなくなったときに表示する理由と、確かめる関数。
pub type Invariant = (&'static str, fn(&mut Repl) -> bool);

pub const INVARIANTS: &[Invariant] = &[
    (
        "env のスタックと宣言ログの長さが一致する",
        |r| r.eng().stack().len() == r.eng().log().len(),
    ),
    // boot が失敗したときだけ env が無い。そのときは宣言も
    // 「環境に含まれている」とは言えないので、log も空でなければならない。
    ("環境が無いなら宣言も持たない", |r| {
        r.eng().env().is_some() || r.eng().log().is_empty()
    }),
    ("持ち越した proofState は今の環境のもの", |r| {
        r.held().is_none_or(|h| h.env == r.eng().env())
    }),
    (
        "証明のスクリプトと巻き戻し用スタックの長さが一致する",
        |r| r.proof().is_none_or(|p| p.script.len() == p.stack.len()),
    ),
    (
        "証明モードの proofState は今のエンジンのもの",
        |r| {
            r.proof()
                .is_none_or(|p| p.generation == r.eng().generation())
        },
    ),
    // Python 版は「プロセスが実行中である」だけを確かめていた。Python 版の kill は
    // 終了を待たないので、中断で止めた直後も poll はまだ None を返し、成り立って
    // いるように見えていた。Rust 版の kill は終了を待つので、止まったままでよい
    // 状態を書く。再起動に失敗して環境が無いときと、replay が中断されて保留が
    // 残っているとき。どちらも次の入力で再起動する。
    (
        "エンジンのプロセスが実行中である (止まっているなら環境が無いか、保留がある)",
        |r| r.eng_mut().alive() || r.eng().env().is_none() || !r.eng().unplayed().is_empty(),
    ),
    (
        "送り方が決まっているならバッファに入力がある",
        |r| r.ready().is_none() || !r.buf().is_empty(),
    ),
    (
        "環境を進めた記録があるなら宣言ログも空でない",
        |r| {
            r.last()
                .is_none_or(|l| !l.advanced || !r.eng().log().is_empty())
        },
    ),
];

// ------------------------------------------------------------------ Driver

/// `Repl` を直接呼ぶ。1 行渡すごとに不変条件を確認する。
pub struct Driver {
    pub repl: Repl,
    out: Sink,
    search: FakeSearch,
    sb: Sandbox,
}

impl Driver {
    /// 既定の環境 (core) で起動する。起動時の表示は捨てる。
    pub fn new() -> Driver {
        Driver::with(None)
    }

    /// 起動時にファイルを `:l` する (`leani file.lean`)。
    pub fn preload(path: &str) -> Driver {
        Driver::with(Some(path))
    }

    fn with(preload: Option<&str>) -> Driver {
        let sb = sandbox();
        let table = load_config(&sb.ctx.config).expect("config");
        let cfg = resolve(&sb.ctx, None, None, &[], &table).expect("resolve");
        let out = Sink::default();
        let search = FakeSearch::default();
        let repl = Repl::new(
            &sb.ctx,
            cfg,
            preload,
            Box::new(out.clone()),
            Box::new(search.clone()),
        )
        .expect("起動できない");
        let mut d = Driver {
            repl,
            out,
            search,
            sb,
        };
        d.check("起動直後");
        d.take();
        d
    }

    /// `:loogle` の応答を決める。
    pub fn answer_loogle(&self, answer: Result<Json, &str>) {
        *self.search.answer.lock().expect("answer") = Some(answer.map_err(String::from));
    }

    /// `:loogle` が問い合わせた回数。
    pub fn loogle_calls(&self) -> usize {
        self.search.calls.load(SeqCst)
    }

    /// テストが一時ファイルを置く場所 (Python 版の `tmp_path`)。
    pub fn tmp(&self) -> &Path {
        self.sb.dir.path()
    }

    pub fn check(&mut self, after: &str) {
        for (why, holds) in INVARIANTS {
            assert!(
                holds(&mut self.repl),
                "{after} で不変条件が成り立たなくなった: {why}"
            );
        }
    }

    /// ためた表示を取り出して空にする。
    fn take(&mut self) -> String {
        strip_ansi(&std::mem::take(&mut *self.out.0.lock().expect("sink")))
    }

    /// 行を順に渡して、出力をまとめて返す。
    pub fn feed(&mut self, lines: &[&str]) -> String {
        self.take();
        for one in lines {
            self.repl.feed(one);
            self.check(&format!("{one:?} のあと"));
        }
        self.take()
    }

    /// 行の代わりに `f` (`Repl` のメソッドなど) を呼んで、出力を返す。
    pub fn feed_with(&mut self, name: &str, f: impl FnOnce(&mut Repl)) -> String {
        self.take();
        f(&mut self.repl);
        self.check(&format!("{name} のあと"));
        self.take()
    }

    /// 複数行を入力して空行で確定させる。
    pub fn block(&mut self, lines: &[&str]) -> String {
        let mut all: Vec<&str> = lines.to_vec();
        all.push("");
        self.feed(&all)
    }

    pub fn declarations(&self) -> Vec<String> {
        self.repl.eng().log().to_vec()
    }

    /// エンジンとのやりとりに差し込み口を入れる (Python 版の `mocker.patch.object`)。
    /// 再起動しても同じ `Engine` なので、差し込み口は残る。
    pub fn hook(&mut self, f: impl FnMut(Stage<'_>) -> Hooked + Send + 'static) {
        self.repl.eng_mut().set_hook(Some(Box::new(f)));
    }

    /// 差し込み口を外す (Python 版の `mocker.stopall`)。
    pub fn unhook(&mut self) {
        self.repl.eng_mut().set_hook(None);
    }
}

// ------------------------------------------------------------------ 差し込み口の部品

/// エンジンが PANIC したときのメッセージ。2 行目以降はバックトレース。
pub const PANIC: &str = "PANIC at Foo.bar Foo:1:2: oops\nbacktrace:\n  ...";

/// リクエストの `cmd`。無ければ空文字列。
pub fn cmd(obj: &Json) -> &str {
    text(obj, "cmd").unwrap_or("")
}

/// 環境を付けずに送るリクエストか。boot と `:l` だけが env を付けない。
pub fn fresh(obj: &Json) -> bool {
    obj.get("cmd").is_some() && obj.get("env").is_none()
}

/// 応答の messages に PANIC を足す。
pub fn with_panic(resp: &Json) -> Json {
    with_info(resp, PANIC)
}

/// 応答の messages に info のメッセージを 1 つ足す。
pub fn with_info(resp: &Json, data: &str) -> Json {
    let mut r = resp.clone();
    let extra = serde_json::json!({ "severity": "info", "data": data });
    match r.get_mut("messages").and_then(Json::as_array_mut) {
        Some(ms) => ms.push(extra),
        None => r["messages"] = serde_json::json!([extra]),
    }
    r
}

/// リクエストがタクティクか (`{"tactic": ..., "proofState": ...}`)。
pub fn is_tactic(obj: &Json) -> bool {
    obj.get("tactic").is_some()
}

/// when(リクエスト) が真になった最初のリクエストでエンジンを止める (Python 版の
/// `query` や `send_tactic` を 1 回だけ `EngineDied` にする差し込み)。
///
/// 差し込み口は再起動しても残るので、2 回目からは通す。
pub fn die_once(d: &mut Driver, when: impl Fn(&Json) -> bool + Send + 'static) {
    let mut dead = false;
    d.hook(move |stage| match stage {
        Stage::Before(obj) if !dead && when(obj) => {
            dead = true;
            Hooked::Kill
        }
        _ => Hooked::Pass,
    });
}

/// when(リクエスト) が真になった最初のリクエストで Ctrl-C を再現する。
///
/// リクエストを書いたあと、レスポンスを読む前に中断する。実際の Ctrl-C も、
/// たいていエンジンが計算している途中、つまりこの位置で起きる。`Before` で
/// 中断すると、`Engine::send` がプロセスを止める処理も、読んでいないレスポンスが
/// パイプに残る状態もテストできない。中断したかを返す。
pub fn interrupt_after_write(
    d: &mut Driver,
    when: impl Fn(&Json) -> bool + Send + 'static,
) -> Arc<AtomicBool> {
    let hit = Arc::new(AtomicBool::new(false));
    let seen = hit.clone();
    d.hook(move |stage| match stage {
        Stage::Written(obj) if !seen.load(SeqCst) && when(obj) => {
            seen.store(true, SeqCst);
            Hooked::Interrupt
        }
        _ => Hooked::Pass,
    });
    hit
}

impl Drop for Driver {
    fn drop(&mut self) {
        self.repl.close();
    }
}
