//! 入力を 1 行ずつ受け取り、エンジンに送って結果を表示する (副作用)。
//!
//! 式と宣言の評価、`:{ ... :}`、エンジンの再起動、init、`:l` `:r` `:reset` `:undo`
//! `:save`、履歴、`:help` `:time` `:restart` `:quit` `:!`、証明モード、`:env`、`:t` `:i`
//! `:p` `:loogle` `:abbrev`、Tab 補完がある。
//!
//! 入力を読むループは `Repl` の外 (`cli::run`) に置く。`Repl` は `feed_line` で 1 行
//! ずつ受け取るだけなので、端末、パイプ、テストのどれからでも同じように呼べる。
//!
//! Tab 補完は `complete` で受け取る。何を補完するかの判定は `leani_core::pure` に、
//! ファイルと PATH を読む候補は `leani_read::complete` にあり、ここではエンジンに
//! 問い合わせる定数名の候補と、そのキャッシュだけを持つ。

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::LazyLock;
use std::sync::atomic::Ordering::SeqCst;
use std::time::Instant;

use leani_core::abbrev::{abbrev_lookup, abbrev_table};
use leani_core::pure::{
    Style, abspath, balanced, block_continues, cache_usable, classify, classify_tac, complete_hits,
    continues, errors, expand_home, full_names, grouped_tactic, has_error, head_line, indent,
    indent_code, int, last_json, lean_str, lean_strs, list, loogle_text, messages, meta_names,
    name_chunk, nested_action, not_evaluable_reason, own_names, save_body, scope_of, sorries,
    splice_sorry, strip_imports, tactic_parse_error, tactic_step, tactic_undo, text, try_this,
};
use leani_core::queries::{COMPLETE_QUERY, DOC_QUERY, META_LINE, PARSE_PROBE, SCOPE_QUERY, fill};
use leani_core::types::{Candidate, EngineFault, Json, Kind, Output, Proof, Scope, State};
use leani_read::complete::candidates;
use leani_read::config::{EnvConfig, config_envs, env_names, load_config, problem, resolve};
use leani_read::places::{COMPLETE_CAP, Ctx};

use crate::boot::prepare;
use crate::engine::{Engine, ExchangeHook, Replay, Undone, strerror};
use crate::history::BlockHistory;
use crate::search::Search;
use crate::show::{columns, panic_check, render};

pub const HELP: &str = "\
式を書くと #eval で評価する。宣言はそのまま実行する。入力が終わったかは Lean のパーサで
判定する。インデントした行か | で始まる行が続くあいだは入力を読み続け、空行で入力を
確定する。確定した直後にインデントした行を書くと、直前の入力の続きとして読み直す。
\\to のような略記は space か Tab で記号に変換する。Tab は記号のあとに空白を入れない。

  :t, :type <expr>    型を表示する (#check)
  :i, :info <name>    型と docstring を表示する
  :p, :print <name>   定義を表示する (#print)
  :loogle <q>         定理を検索する (外部サービスに問い合わせる)
                      例: :loogle |- ?a + ?b = ?b + ?a
                      名前、型、結論 (|- を付ける)、名前に含まれる語 (\"commutative\")
  :l, :load <file>    ファイルを読み込む (実行した宣言は捨てる)
  :r, :reload         ファイルを読み込み直す (読み込んでいなければ :reset)
  :reset              宣言をすべて取り消す (:l したあとは読み込んだ直後に戻る)
  :undo [n]           直前の n 件の宣言を取り消す
  :env [name]         今の環境を表示する / 設定した環境に切り替えて再起動する
  :prove [n]          n 番目の sorry の証明モードを始める
  :goals, :g          残っている sorry とゴールを表示する
  :save <file>        実行した宣言を .lean に書き出す (:l で読み込める)
  :time               実行時間の表示を切り替える
  :{ ... :}           複数行を明示的に囲む
  :! <cmd>            shell のコマンドを実行する
  :abbrev [s]         略記の一覧を表示する
                      s が記号ならその記号の略記、それ以外なら s で始まる略記
  :restart            エンジンを再起動して、宣言を再実行する
  :help, :h, :?       このヘルプを表示する
  :q, :quit           終了する (Ctrl-D)

証明モード (⊢>) では 1 回の入力が 1 タクティクになる。:goals :script :undo :done を
使える。証明モードの :help は、証明モードのコマンドだけを表示する。
";

pub const PROOF_HELP: &str = "証明モード: 1 回の入力が 1 タクティク。:goals ゴール  :script スクリプト  :undo 取り消す  :done 終了";

/// Tab 補完に出す `:` コマンドの名前。ヘルプから取り出す。
pub static META_NAMES: LazyLock<Vec<String>> = LazyLock::new(|| meta_names(HELP));

/// 再実行できなかった宣言は `Engine` の unplayed に保留する。その案内。
pub const PENDING_HINT: &str = "これらの宣言は環境に追加せず、保留にした。:restart で再実行できる";

/// `:prove` を待っている sorry と、その sorry を含む宣言。
///
/// 保留するのは直前の宣言の sorry だけである。環境を変える操作は、保留を捨てるか、
/// 今の環境の宣言の sorry で作り直す。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub sorries: Vec<Json>,
    /// sorry を含む宣言のソース。
    pub src: String,
    /// その宣言が作った env id。`:undo` などで環境が変わっていたら、sorry を置き換える
    /// ときに宣言を取り消さない (二重に pop して手前の宣言が消えるため)。
    pub env: Option<i64>,
    /// `:prove` で選んだ sorry (位置つき)。
    pub at: Option<Json>,
}

/// 直前に送った入力。インデント行を受け取ったときに直前の入力に戻れるよう保存しておく。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Last {
    pub src: String,
    /// 環境を進めたか (取り消すべきか)。
    pub advanced: bool,
    /// 証明モードでのタクティクだったか。
    pub proof: bool,
    pub env_before: Option<i64>,
}

/// rewind で取り消したもの。書き直さずにやめたら `restore_undone` が戻す。
#[derive(Clone, Debug)]
enum Rewound {
    /// 取り消した宣言と、その宣言の sorry の保留。
    Decl(Undone, Option<Held>),
    /// タクティクを取り消す前の証明モードの状態。
    Tactic(Proof),
}

/// `feed_*` が 1 行をどう扱ったか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// buf に追加したので、完結したかをパーサに問い合わせる。
    Probe,
    /// この行の処理は終わった。
    Done,
    /// `:quit`。ループを抜ける。
    Quit,
}

/// `Engine::new` がエンジンを用意する進み具合を `Output` に流す。
struct ToOutput<'a>(&'a mut dyn Output);

impl io::Write for ToOutput<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.put(&String::from_utf8_lossy(buf));
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// どのプロセスのどの環境か (世代, env)。補完のキャッシュと open の一覧が、
/// 問い合わせたときの環境を覚えるのに使う。
type EnvAt = (u64, Option<i64>);

/// 1 つのセッション。
///
/// セッションの状態 (入力バッファ、直前の入力、証明モード、保留した sorry) を持つのは
/// `Engine` とこの struct だけ。`Engine` の状態は `Engine` のメソッドだけが書き換え、
/// `Repl` は `Engine` のフィールドに直接触らない。
pub struct Repl {
    ctx: Ctx,
    out: Box<dyn Output>,
    style: Style,
    eng: Engine,
    /// `:loogle` の問い合わせ先。テストでは偽物に差し替える。
    search: Box<dyn Search>,

    // 補完のキャッシュ
    /// `name_chunk` ごとの定数名と、取得したときの (世代, env) (`chunks` を参照)。
    comp_cache: HashMap<String, (EnvAt, Vec<String>)>,
    /// 今の namespace と open。(世代, env) が変わったら問い合わせ直す。
    scope: Option<(EnvAt, Scope)>,
    /// REPL で実行した宣言の名前と、集めたときのログ。ログが変わったら集め直す。
    /// 長さだけを覚えると、`:undo` のあとに同じ数の宣言を足したときに気づかない。
    own: (Option<Vec<String>>, Vec<String>),

    // 入力バッファ
    buf: Vec<String>,
    /// 構文的に完結しているときの送り方。
    ready: Option<Kind>,
    last: Option<Last>,
    /// `:{ ... :}` の中か。
    explicit: bool,
    /// rewind で取り消したもの。
    undone: Option<Rewound>,
    show_time: bool,
    /// このセッションで `:save` したパス。同じパスへの 2 回目以降は上書きする。
    saved: HashSet<String>,
    /// 標準入力と標準出力が端末のときだけ履歴を残す。パイプからの入力は履歴にしない。
    history: Option<BlockHistory>,

    // 証明モード
    proof: Option<Proof>,
    held: Option<Held>,

    /// `:env` で元の環境にも戻れなくなった理由。環境が無いまま続けても、どの入力も
    /// エラーになるだけなので、`feed` はループを抜けるよう true を返す。端末はこの
    /// 理由を表示して終了する。
    lost: Option<String>,
    /// `:env` の切り替えで boot に失敗して終了させたエンジン。プロセスを残して
    /// いないことをテストが確かめる。
    #[cfg(feature = "testing")]
    failed: Vec<Engine>,
}

impl Repl {
    /// エンジンを起動して起点の環境を作る。表示はすべて `out` に出す。
    /// `preload` は起動時に `:l` するファイル (`leani file.lean`)。`search` は
    /// `:loogle` の問い合わせ先。
    pub fn new(
        ctx: &Ctx,
        cfg: EnvConfig,
        preload: Option<&str>,
        mut out: Box<dyn Output>,
        search: Box<dyn Search>,
    ) -> Result<Repl, EngineFault> {
        let style = Style { color: ctx.color };
        let eng = boot_engine(ctx, cfg, &mut None, out.as_mut(), style).map_err(|(e, _)| e)?;

        let mut repl = Repl {
            ctx: ctx.clone(),
            out,
            style,
            eng,
            search,
            comp_cache: HashMap::new(),
            scope: None,
            own: (None, Vec::new()),
            buf: Vec::new(),
            ready: None,
            last: None,
            explicit: false,
            undone: None,
            show_time: false,
            saved: HashSet::new(),
            history: history_for(ctx),
            proof: None,
            held: None,
            lost: None,
            #[cfg(feature = "testing")]
            failed: Vec::new(),
        };
        repl.begin(preload);
        Ok(repl)
    }

    /// 起動した直後にすることを行う。init を実行し、`preload` があれば `:l` する。
    fn begin(&mut self, preload: Option<&str>) {
        if Path::new(&self.ctx.init).is_file() {
            self.apply_init();
        }
        if let Some(path) = preload {
            self.load(path, true);
        }
    }

    /// 新しいエンジンに入れ替えて、セッションの状態を起動した直後に戻す。
    fn install(&mut self, eng: Engine, preload: Option<&str>) {
        self.eng = eng;
        self.forget();
        self.begin(preload);
    }

    /// セッションの状態を起動した直後に戻す。補完のキャッシュも捨てる (別の
    /// エンジンの環境のものなので)。
    fn forget(&mut self) {
        self.buf.clear();
        self.ready = None;
        self.last = None;
        self.explicit = false;
        self.undone = None;
        self.show_time = false;
        self.proof = None;
        self.held = None;
        self.comp_cache.clear();
        self.scope = None;
        self.own = (None, Vec::new());
    }

    // -- 読むためのメソッド -------------------------------------------------

    pub fn eng(&self) -> &Engine {
        &self.eng
    }

    /// テストが `Engine` の差し込み口や `alive` を使うため。
    #[cfg(feature = "testing")]
    #[doc(hidden)]
    pub fn eng_mut(&mut self) -> &mut Engine {
        &mut self.eng
    }

    pub fn held(&self) -> Option<&Held> {
        self.held.as_ref()
    }

    pub fn proof(&self) -> Option<&Proof> {
        self.proof.as_ref()
    }

    pub fn ready(&self) -> Option<Kind> {
        self.ready
    }

    pub fn buf(&self) -> &[String] {
        &self.buf
    }

    pub fn last(&self) -> Option<&Last> {
        self.last.as_ref()
    }

    pub fn explicit(&self) -> bool {
        self.explicit
    }

    /// 入力の履歴。端末 (`term`) が Ctrl-P で呼び出すのに使う。端末でなければ None。
    pub fn history(&self) -> Option<&BlockHistory> {
        self.history.as_ref()
    }

    pub fn show_time(&self) -> bool {
        self.show_time
    }

    /// `:env` で元の環境にも戻れなくなった理由。`feed` が true を返したあとに
    /// 呼び出し元が確かめ、あれば表示して終了する。
    pub fn env_lost(&self) -> Option<&str> {
        self.lost.as_deref()
    }

    /// `:env` の切り替えで boot に失敗したエンジン。テストだけが使う。
    #[cfg(feature = "testing")]
    #[doc(hidden)]
    pub fn failed_engines_mut(&mut self) -> &mut [Engine] {
        &mut self.failed
    }

    /// init ファイルのパスを差し替える。テストだけが使う。
    #[cfg(feature = "testing")]
    #[doc(hidden)]
    pub fn set_init(&mut self, path: &str) {
        self.ctx.init = path.to_string();
    }

    /// rewind で取り消した宣言を直接置く。テストだけが使う。
    #[cfg(feature = "testing")]
    #[doc(hidden)]
    pub fn set_undone(&mut self, undone: Undone) {
        self.undone = Some(Rewound::Decl(undone, None));
    }

    /// 次の入力のプロンプト。ブロックの途中なら ` | `、証明モードでは `⊢> `。
    pub fn prompt(&self) -> String {
        match self.proof {
            _ if self.pending() => self.style.dim(" | "),
            Some(_) => self.style.c("35", "⊢> "),
            None => self.style.c("36", &self.eng.cfg().prompt),
        }
    }

    /// 入力の途中か (ブロックの続きの行か、`:{ ... :}` の中)。
    pub fn pending(&self) -> bool {
        !self.buf.is_empty() || self.explicit
    }

    /// 表示の出口。ループが改行や、行を読めなかったことを表示するのに使う。
    pub fn out(&mut self) -> &mut dyn Output {
        self.out.as_mut()
    }

    /// Ctrl-C のフラグを読んで消す。消さないと、そのあとの再起動まで中断される。
    pub fn take_interrupt(&self) -> bool {
        self.ctx.interrupted.swap(false, SeqCst)
    }

    /// Ctrl-C で入力中のブロックを捨てる。直前の入力の続きとしても読み直さない。
    pub fn cancel(&mut self) {
        self.discard();
        self.last = None;
        self.out.write("^C");
    }

    // -- 補完 -------------------------------------------------------------

    /// カーソルの前の文字列 `text` に対する Tab 補完の候補。
    ///
    /// 定数名はエンジンに問い合わせるので、`:` コマンドや略記の途中では問い合わせない
    /// (`leani_read::complete::candidates` が、必要なときだけ `complete_names` を呼ぶ)。
    pub fn complete(&mut self, text: &str) -> Vec<Candidate> {
        let ctx = self.ctx.clone();
        candidates(
            &ctx,
            text,
            &mut |prefix| self.complete_names(prefix),
            &|| config_envs(&ctx),
            &META_NAMES,
        )
    }

    /// 今の環境で短い名前で書ける定数のうち、prefix で始まるもの。
    ///
    /// `open Lean` のあとの `Json.pa` は `Lean.Json.pa` として探し、`Lean.` を
    /// 取り除いて返す。namespace の中なら、その名前空間と親の名前空間からも同じ
    /// ように探す。
    pub fn complete_names(&mut self, prefix: &str) -> Vec<String> {
        if prefix.chars().count() < 2 {
            return Vec::new();
        }
        let scope = self.scope_now();
        let keys: BTreeSet<String> = full_names(prefix, &scope.opens)
            .iter()
            .map(|(_, full)| name_chunk(full).to_string())
            .collect();
        let chunks = self.chunks(&keys);
        let own = self.own_names_now();
        complete_hits(prefix, &scope, &chunks, &own)
    }

    /// 名前空間ごとの定数名の一覧。キャッシュに無いものは、定数を 1 回走査して
    /// まとめて取得する。
    ///
    /// キャッシュは、取得したときの環境が今の環境の元になっている間だけ使う
    /// (`cache_usable`)。宣言を 1 つ実行するたびにキャッシュを捨てると Mathlib では
    /// 毎回 1.1 秒かかるので、REPL で実行した宣言の名前だけを leani 側で追加する。
    /// open する名前空間が増えても、定数の走査は 1 回で済ませる。問い合わせ直しに
    /// 失敗したときも、使えないキャッシュは返さない。
    fn chunks(&mut self, keys: &BTreeSet<String>) -> BTreeMap<String, Vec<String>> {
        let now = (self.eng.generation(), self.eng.env());
        let missing: Vec<String> = keys
            .iter()
            .filter(|k| self.cached(k).is_none())
            .cloned()
            .collect();
        if !missing.is_empty() {
            let query = fill(
                COMPLETE_QUERY,
                &[&lean_strs(&missing), &COMPLETE_CAP.to_string()],
            );
            let out = self.quiet(|eng| eng.query(&query)).flatten();
            let got = last_json(out.as_deref());
            if let Some(arrays) = got.as_ref().and_then(Json::as_array)
                && arrays.len() == missing.len()
            {
                for (k, names) in missing.iter().zip(arrays) {
                    let names: Vec<String> = list_of_strings(names);
                    self.comp_cache.insert(k.clone(), (now, names));
                }
            }
        }
        keys.iter()
            .filter_map(|k| self.cached(k).map(|names| (k.clone(), names.clone())))
            .collect()
    }

    /// `key` のキャッシュ。取得したときの環境が今の環境の元になっていなければ None。
    fn cached(&self, key: &str) -> Option<&Vec<String>> {
        let (at, names) = self.comp_cache.get(key)?;
        let eng = &self.eng;
        cache_usable(*at, eng.generation(), eng.env(), eng.stack()).then_some(names)
    }

    /// 今の namespace と open。環境が変わったときだけ問い合わせ直す。
    ///
    /// open は宣言と同じく環境ごとに repl が保存しているので、入力した文字列から
    /// 解析するより正確に取得できる (`open X in` は含まれず、`hiding` や
    /// `renaming` も反映される)。この問い合わせは定数を走査しないので、Mathlib
    /// でも数十ミリ秒で終わる。
    fn scope_now(&mut self) -> Scope {
        let key = (self.eng.generation(), self.eng.env());
        match &self.scope {
            Some((at, scope)) if *at == key => scope.clone(),
            _ => {
                let out = self.quiet(|eng| eng.query(SCOPE_QUERY)).flatten();
                let scope = scope_of(last_json(out.as_deref()).as_ref());
                self.scope = Some((key, scope.clone()));
                scope
            }
        }
    }

    /// 補完の問い合わせ。`guard` と違って、エンジンが異常終了していても再起動しない。
    ///
    /// 補完は行編集の途中 (端末が raw モードのとき) に呼ばれる。そこで再起動すると、
    /// 再起動の表示と replay の結果が入力の行に混ざり、証明モードや直前の入力の状態も
    /// 失われる。候補を返さずに、再起動は次の入力の `guard` に任せる。
    fn quiet<T>(&mut self, f: impl FnOnce(&mut Engine) -> Result<T, EngineFault>) -> Option<T> {
        f(&mut self.eng).ok()
    }

    /// REPL で実行した宣言の名前。ログが変わったときだけ集め直す。
    fn own_names_now(&mut self) -> Vec<String> {
        let log = self.eng.log();
        if self.own.0.as_deref() != Some(log) {
            self.own = (Some(log.to_vec()), own_names(log));
        }
        self.own.1.clone()
    }

    // -- 起動 -------------------------------------------------------------

    /// init ファイルを起動直後の環境で実行する。
    ///
    /// `:l` と違って環境を作り直さない。ここで実行した宣言は base に含めるので
    /// `:reset` しても消えない (GHCi の .ghci と同じ扱い)。
    pub fn apply_init(&mut self) {
        let init = self.ctx.init.clone();
        let text = match std::fs::read_to_string(&init) {
            Ok(text) => text,
            Err(e) => {
                let why = strerror(&e);
                self.out
                    .fail(&self.style.red(&format!("{init} が読めない ({why})")));
                return;
            }
        };

        let src = strip_imports(&text);
        if src.trim().is_empty() {
            return;
        }

        let Some(resp) = self.guard_quiet(|eng| eng.run_init(&src)) else {
            return;
        };
        if has_error(&resp) {
            self.out
                .fail(&self.style.red(&format!("{init} にエラーがある:")));
            render(self.out.as_mut(), self.style, &resp, &src, 0, 0);
        } else {
            self.out
                .write(&self.style.dim(&format!("-- {init} を読み込んだ")));
        }
    }

    /// 履歴に 1 件として追加する。複数行の宣言も全体で 1 件になる。
    fn remember(&mut self, src: &str) {
        if let Some(history) = &self.history
            && !src.trim().is_empty()
        {
            history.record(src.trim_end());
        }
    }

    // -- 再起動 -----------------------------------------------------------

    /// エンジンを再起動して宣言を replay する。再起動できなかったときと、replay を
    /// 途中で打ち切ったとき (Ctrl-C か異常終了) は None を返す。
    ///
    /// boot が失敗する (import に問題がある / Ctrl-C で中断した) ことはよくある。
    /// 宣言は `Engine` 側に残るので、ユーザーは原因を直してから `:restart` で再実行
    /// できる。ここでエラーを返すとセッションごと終了するので、エラーは返さない。
    fn revive(&mut self) -> Option<Replay> {
        let out = match self.eng.restart() {
            Ok(out) => out,
            Err(EngineFault::Interrupted) => {
                self.out.write(
                    &self
                        .style
                        .yellow("^C 再起動を中断した。:restart でやり直せる"),
                );
                self.fold_proof();
                return None;
            }
            Err(e) => {
                let why = reason(&e);
                self.out.fail(
                    &self
                        .style
                        .red(&format!("エンジンを再起動できなかった: {why}")),
                );
                self.out
                    .write(&self.style.dim("  原因を直してから :restart"));
                self.fold_proof();
                return None;
            }
        };

        self.proof = None;
        self.last = None;
        self.clear_pending();
        self.out.write(&self.style.dim(&format!(
            "宣言 {} 件を再実行した (env {})",
            out.done.len(),
            env_text(self.eng.env())
        )));
        if out.interrupted {
            self.out.write(&self.style.yellow("^C 再実行を中断した"));
        }
        self.report_replay(&out);
        self.reattach(&out);
        if out.skipped.is_empty() {
            Some(out)
        } else {
            None
        }
    }

    /// 再起動に失敗したときの後始末。環境が無いので、直前の入力に戻ることもできない。
    ///
    /// 再起動に失敗したときも証明モードを終了する。終了しないと、無効になった
    /// proofState を保持したままになり、leani はどの行にも赤い
    /// "Unknown proof state." だけを返す証明モードのままになる。
    fn fold_proof(&mut self) {
        self.last = None;
        if self.proof.is_some() {
            self.drop_proof();
        }
        self.proof = None;
        // 保留している proofState も終了したプロセスのもの。残すと :goals が
        // 環境に無い宣言のゴールを表示し、:prove がその無効な proofState で証明モードを
        // 始める。
        self.clear_pending();
    }

    /// replay で再実行できなかったものを報告する。表示しないと、ユーザーは気付けない。
    fn report_replay(&mut self, out: &Replay) {
        for note in &out.notes {
            self.out.write(&self.style.yellow(note));
        }
        for src in &out.failed {
            self.out.write(&self.style.yellow(&format!(
                "再実行に失敗した宣言: {}",
                head_line(src, HEAD_WIDTH)
            )));
        }
        if !out.skipped.is_empty() {
            self.out.write(
                &self
                    .style
                    .yellow(&format!("再実行しなかった宣言: {} 件", out.skipped.len())),
            );
        }
        if !out.failed.is_empty() || !out.skipped.is_empty() {
            self.out
                .write(&self.style.dim(&format!("  {PENDING_HINT}")));
        }
    }

    /// replay で戻した宣言に sorry が残っていたら、`:prove` を使えるようにする。
    ///
    /// タクティクの実行中にエンジンが異常終了したときや Ctrl-C を押したとき、証明
    /// していた宣言は replay で環境に戻っている。新しい proofState を取得し直さないと、
    /// 宣言はあるのに `:prove` が「sorry が無い」と表示するだけになり、ユーザーは
    /// `:undo` するしかなくなる。
    ///
    /// 戻した最後の宣言に sorry が無ければ、保留も捨てる。`submit_cmd` と同じく、
    /// 保留するのは直前の宣言の sorry だけである。残すと、前の宣言の proofState で
    /// `:prove` が証明モードを始める。
    fn reattach(&mut self, out: &Replay) {
        self.held = None;
        let Some(src) = out.done.last() else {
            return;
        };
        if out.sorries.is_empty() {
            return;
        }

        self.held = Some(Held {
            sorries: out.sorries.clone(),
            src: src.clone(),
            env: self.eng.env(),
            at: None,
        });
        self.out.write(&self.style.dim(&format!(
            "-- :prove で証明モードを再開できる (sorry {} 個)",
            out.sorries.len()
        )));
    }

    /// 入力した宣言を今のエンジンで再実行する。`:env` の切り替え後と、エンジンの
    /// 再起動で env id が無効になった宣言をテキストから戻すときに使う。
    pub fn replay_into(&mut self, log: &[String]) {
        if log.is_empty() {
            return;
        }

        if self.eng.env().is_none() {
            // ここはユーザーに案内を表示する層。Engine のチェックで出るエラーを
            // そのまま「内部エラー」として表示すると、テキストが残っていることも、
            // 次に何をすればよいかもユーザーに伝わらない。宣言を保留にするのは
            // Engine::replay に任せ、案内だけをここで表示する。
            self.eng.replay(log);
            self.out.write(&self.style.yellow(&format!(
                "環境が無いので、宣言 {} 件を再実行できなかった",
                log.len()
            )));
            self.out
                .write(&self.style.dim(&format!("  {PENDING_HINT}")));
            return;
        }

        let Some(out) = self.guard_quiet(|eng| Ok(eng.replay(log))) else {
            self.out.write(
                &self
                    .style
                    .yellow(&format!("宣言 {} 件を再実行できなかった", log.len())),
            );
            return;
        };

        self.out.write(&self.style.dim(&format!(
            "宣言 {} 件を再実行した (env {})",
            out.done.len(),
            env_text(self.eng.env())
        )));
        self.report_replay(&out);
        self.reattach(&out);
    }

    /// エンジンが異常終了したり中断されたりしたら、再起動して replay する。
    ///
    /// 異常終了した場合は、再起動したうえで `f` を 1 回だけ再実行する。ユーザーが
    /// Ctrl-C で止めた場合は再実行しない (ユーザーは止めたいので)。再起動の replay を
    /// 途中で打ち切ったときも再実行しない。Ctrl-C で止めたならユーザーは止めたいので、
    /// 異常終了したなら同じエンジンに送っても失敗するので。
    ///
    /// 再実行でもエンジンが終了したら、もう一度だけ再起動して、再実行はしない。
    /// 同じ入力でエンジンが毎回異常終了する場合がある。再起動しないと、終了した
    /// プロセスが残り、次の入力もすべて失敗する。
    ///
    /// それ以外のエラー (`NoEnvironment`、`Io`、`Setup`) は「内部エラー」として表示し、
    /// None を返す。
    ///
    /// 異常終了したのに再実行できなかったときは、入力を実行しなかったことを表示する。
    /// 表示しないと、ユーザーは入力が実行されたのかどうか分からない。
    fn guard<T>(&mut self, f: impl Fn(&mut Engine) -> Result<T, EngineFault>) -> Option<T> {
        self.guard_with(f, true)
    }

    /// `guard` と同じだが、入力を実行しなかったことは表示しない。呼び出し元が
    /// 自分で表示するときと、ユーザーの入力ではないものを送るときに使う。
    fn guard_quiet<T>(&mut self, f: impl Fn(&mut Engine) -> Result<T, EngineFault>) -> Option<T> {
        self.guard_with(f, false)
    }

    fn guard_with<T>(
        &mut self,
        f: impl Fn(&mut Engine) -> Result<T, EngineFault>,
        tell: bool,
    ) -> Option<T> {
        let retry = match f(&mut self.eng) {
            Ok(v) => return Some(v),
            Err(EngineFault::Interrupted) => {
                self.out
                    .fail(&self.style.yellow("^C 中断した。エンジンを再起動する…"));
                false
            }
            Err(EngineFault::Died(_)) => {
                self.out
                    .write(&self.style.red("エンジンが異常終了した。再起動する…"));
                true
            }
            Err(e) => {
                self.internal_error(&e);
                return None;
            }
        };

        if self.revive().is_none() {
            if retry && tell {
                self.out
                    .write(&self.style.dim("  入力した行は実行しなかった"));
            }
            return None;
        }

        if retry {
            match f(&mut self.eng) {
                Ok(v) => return Some(v),
                Err(EngineFault::Interrupted) => {
                    self.out
                        .fail(&self.style.yellow("^C 中断した。エンジンを再起動する…"));
                }
                Err(EngineFault::Died(_)) => {
                    self.out.fail(&self.style.red(
                        "再実行してもエンジンが異常終了した。この入力は実行せずに、もう一度再起動する…",
                    ));
                }
                Err(e) => {
                    self.internal_error(&e);
                    return None;
                }
            }
            self.revive();
        }

        None
    }

    /// 想定外のエラーでも、その 1 行のエラーとして扱う。セッションを終了すると
    /// それまでの宣言をすべて失うので、それが一番大きな損失になる。
    fn internal_error(&mut self, e: &EngineFault) {
        let name = match e {
            EngineFault::Setup(_) => "EngineError",
            EngineFault::Died(_) => "EngineDied",
            EngineFault::Interrupted => "Interrupted",
            EngineFault::NoEnvironment(_) => "NoEnvironment",
            EngineFault::Io(_) => "OSError",
        };
        self.out
            .fail(&self.style.red(&format!("内部エラー: {name}: {e}")));
    }

    // -- 完結判定 ---------------------------------------------------------

    /// src が command、term、タクティクの列として読めるかを Lean のパーサに問い合わせる。
    /// 1 回のやりとりで 3 つとも取得する。実行はしない。ユーザー定義の notation も
    /// 認識される。
    ///
    /// guard が結果を返さなかったら None を返す。Ctrl-C で止めたとき、再起動か
    /// 再起動のあとの再実行が止まったとき、内部エラーのときである。呼び出し元は
    /// 入力を送らずに捨てる。問い合わせがエラーになったときと環境が無いときは
    /// `Some(None)` を返す。この 2 つを混ぜると、環境が無いときにも入力を捨てる
    /// ことになる。
    fn parse(&mut self, src: &str) -> Option<Option<Json>> {
        let probe = fill(PARSE_PROBE, &[&lean_str(src)]);
        let got = self.guard_quiet(|eng| eng.query(&probe))?;
        Some(last_json(got.as_deref()).filter(Json::is_object))
    }

    /// 完結したかと、送り方。guard が結果を返さなかったら None。
    fn probe(&mut self, src: &str) -> Option<(State, Kind)> {
        self.parse(src).map(|p| classify(p.as_ref()))
    }

    fn probe_tac(&mut self, src: &str) -> Option<(State, Kind)> {
        self.parse(src).map(|p| classify_tac(p.as_ref()))
    }

    // -- ループから呼ぶもの -----------------------------------------------

    /// 端末から受け取った 1 件を処理する。履歴から呼び出した入力は改行を含む。
    /// true を返したらループを抜ける。
    ///
    /// 想定外の panic でも、その 1 件のエラーとして扱ってセッションを続ける。
    /// セッションを終了するとそれまでの宣言をすべて失うので、それが一番大きな
    /// 損失になる。リクエストを書いてから応答を読むまでのあいだに panic すると、
    /// 読んでいない応答が残って次の応答とずれるので、エンジンを再起動して宣言を
    /// replay する。
    pub fn feed_line(&mut self, line: &str) -> bool {
        match catch_unwind(AssertUnwindSafe(|| self.feed_lines(line))) {
            Ok(quit) => quit,
            Err(payload) => {
                let why = panic_text(payload.as_ref());
                self.out
                    .fail(&self.style.red(&format!("内部エラー: {why}")));
                self.discard();
                self.out.write(&self.style.dim("エンジンを再起動する…"));
                self.revive();
                false
            }
        }
    }

    fn feed_lines(&mut self, line: &str) -> bool {
        // 1 件にまとめた履歴を呼び出すと改行を含む 1 行として返ってくるので、
        // 行に分けて、入力したときと同じ順に feed へ渡す。
        let lines: Vec<&str> = line.split('\n').collect();
        for one in &lines {
            if self.feed(one) {
                return true;
            }
        }

        // 呼び出した複数行は全体で 1 件なので、末尾に空行を渡して確定させる。
        // ブロックが途中なら確定させず、続きの行を待つ。Ctrl-C で捨てた入力は
        // 途中のまま履歴に残るので、呼び出して続きを書けるようにする。
        if lines.len() > 1 && self.ready.is_some() {
            self.feed("")
        } else {
            false
        }
    }

    /// 入力中のブロックを捨てる。Ctrl-C で呼ぶ。
    pub fn discard(&mut self) {
        // 入力した内容は履歴に残す。捨てるのは入力バッファであって、入力の記録
        // ではない。長い宣言の入力を間違えたときに、ユーザーは Ctrl-P で呼び出せる。
        let src = self.buf.join("\n");
        self.remember(&src);
        self.buf.clear();
        self.ready = None;
        self.explicit = false;
        self.restore_undone();
    }

    /// 入力の終わりで、入力途中のブロックを確定する。空行を入力したときと同じ。
    /// 入力の終わりは、パイプの EOF、端末の Ctrl-D、ノートブックのセルの終わり。
    /// `:{` が閉じていなければ、`:{` から後ろを捨てて、そのことを表示する。
    ///
    /// 確定は `feed_line` を通すので、panic しても 1 件のエラーとして扱う。
    pub fn end_block(&mut self) {
        if self.explicit {
            self.out
                .fail(&self.style.red(":} が無いので、:{ から後ろは実行しなかった"));
            self.discard();
        } else if !self.buf.is_empty() {
            self.feed_line("");
        }
    }

    pub fn close(&mut self) {
        self.eng.kill();
    }

    /// 1 行受け取る。true を返したらループを抜ける。
    ///
    /// 完結したかはパーサに問い合わせるが、それだけでは足りない。Lean では
    /// `structure P where` や `def f := 1` はそれ自体で完結した command なので、
    /// パーサは「完結した」と返す。それでも次のインデント行は続きになりうる。
    /// そこで次のように判定する:
    ///
    /// * パーサが「途中」と返したら継続行を読む (ブロックを始める)。
    /// * ブロックの中では、インデント行が続く限り読む。空行を受け取ったら確定する。
    ///   パーサが「完結」と返したあとなら、インデントの無い行を受け取っても確定する。
    /// * 確定した直後にインデント行を受け取ったら、直前の入力の続きとして読み直す
    ///   (環境も 1 つ戻す)。
    pub fn feed(&mut self, line: &str) -> bool {
        let step = if self.explicit {
            self.feed_explicit(line)
        } else if self.buf.is_empty() {
            self.feed_first(line)
        } else {
            self.feed_more(line)
        };

        match step {
            Step::Quit => return true,
            Step::Done => return false,
            Step::Probe => {}
        }

        let src = self.buf.join("\n");
        let proving = self.proof.is_some();
        let judged = if proving {
            self.probe_tac(&src)
        } else {
            self.probe(&src)
        };
        let left_proof = proving && self.proof.is_none();
        let Some((state, kind)) = judged.filter(|_| !left_proof) else {
            // 判定を止めた (Ctrl-C) ので、ユーザーは入力を送りたくない。判定の結果が
            // 無いまま送ると、式も宣言として送ることになる。
            //
            // 判定の途中で guard がエンジンの異常終了を検出し、revive が証明モードを
            // 終了したときも送らない。このまま submit すると proof が None なので
            // command として処理され、タクティクの行が宣言として送られて
            // "unexpected identifier; expected command" になる。さらに
            // submit_cmd の clear_pending が reattach で設定した値を消すので、直前に
            // 表示した「:prove で再開できる」も正しくなくなる。
            //
            // 入力した行は送らずに捨て、案内だけを表示する。
            self.discard();
            self.out
                .write(&self.style.dim("  入力した行は実行しなかった"));
            return false;
        };
        if state == State::More {
            self.ready = None;
        } else if block_continues(&self.buf, &src) {
            self.ready = Some(kind); // 完結。ただし続きがありうるので空行を待つ
        } else {
            self.buf.clear();
            self.ready = None;
            self.submit(&src, Some(kind));
        }
        false
    }

    /// `:{ ... :}` の中の行。`:}` を受け取るまで判定せずに buf に追加する。
    fn feed_explicit(&mut self, line: &str) -> Step {
        if line.trim() == ":}" {
            let src = self.buf.join("\n");
            self.buf.clear();
            self.explicit = false;
            if !src.trim().is_empty() {
                self.submit(&src, None);
            }
        } else {
            self.buf.push(line.to_string());
        }
        Step::Done
    }

    /// バッファが空のときの 1 行目。
    fn feed_first(&mut self, line: &str) -> Step {
        let s = line.trim();
        if s.is_empty() {
            self.last = None;
            Step::Done
        } else if s == ":{" {
            self.explicit = true;
            self.buf.clear();
            Step::Done
        } else if s.starts_with(':') {
            self.last = None;
            self.remember(s);
            if self.meta(s) { Step::Quit } else { Step::Done }
        } else if continues(line) && self.last.is_some() {
            // 確定した入力の続きだった。直前の入力を取り消して、続きとして読み直す。
            self.rewind();
            let last = self.last.take().expect("last を確かめた");
            self.buf = last.src.lines().map(String::from).collect();
            self.buf.push(line.to_string());
            Step::Probe
        } else {
            self.buf = vec![line.to_string()];
            Step::Probe
        }
    }

    /// ブロックの 2 行目以降。
    fn feed_more(&mut self, line: &str) -> Step {
        if line.trim().is_empty() {
            // 空行で確定
            let src = self.buf.join("\n").trim_end().to_string();
            let kind = self.ready;
            self.buf.clear();
            self.ready = None;
            if !src.trim().is_empty() {
                self.submit(&src, kind);
            }
            Step::Done
        } else if META_LINE.is_match(line) {
            // ブロックの入力をやめる
            let src = self.buf.join("\n");
            let kind = self.ready;
            self.buf.clear();
            self.ready = None;
            match kind {
                Some(kind) => self.submit(&src, Some(kind)),
                None => {
                    self.remember(&src);
                    self.restore_undone();
                    self.out
                        .write(&self.style.dim("-- 入力が途中だったので捨てた"));
                    self.last = None;
                }
            }
            if self.feed(line) {
                Step::Quit
            } else {
                Step::Done
            }
        } else if self.ready.is_some() && !continues(line) {
            // 確定済みのブロックのあとにインデントの無い行を受け取った。
            // ブロックを送ってから、その行を読み直す
            let src = self.buf.join("\n");
            let kind = self.ready;
            self.buf.clear();
            self.ready = None;
            self.submit(&src, kind);
            if self.feed(line) {
                Step::Quit
            } else {
                Step::Done
            }
        } else {
            self.buf.push(line.to_string());
            Step::Probe
        }
    }

    /// 直前の入力を取り消して、続きを書けるようにする。
    fn rewind(&mut self) {
        let Some(last) = self.last.as_ref() else {
            return;
        };
        if !last.advanced {
            return;
        }
        // 書き直しをやめたときに戻せるよう保存しておく
        // (Ctrl-C / ブロックの入力をやめたとき)。
        if last.proof {
            // 証明モードのタクティクは環境を進めていない。証明モードが終了していても、
            // ここで宣言を pop してはいけない (直前の実際の宣言が消える)。
            if let Some(proof) = self.proof.take() {
                self.proof = tactic_undo(&proof).or_else(|| Some(proof.clone()));
                self.undone = Some(Rewound::Tactic(proof));
            }
            return;
        }
        // 取り消した宣言の sorry の保留も、今の環境のものではなくなるので一緒に保存する。
        let undone = self.eng.pop_decl();
        self.undone = Some(Rewound::Decl(undone, self.held.take()));
    }

    /// rewind で取り消したものを戻す。書き直さずにやめたとき。
    pub fn restore_undone(&mut self) {
        match self.undone.take() {
            None => {}
            Some(Rewound::Tactic(proof)) => {
                // 保存しているあいだにエンジンが再起動されたら、保存した proofState は
                // 前のプロセスのものなので戻さない。証明モードが終了していたら、
                // 終了した証明モードを戻すことになるので、これも戻さない。
                if proof.generation == self.eng.generation() && self.proof.is_some() {
                    self.proof = Some(proof);
                }
            }
            Some(Rewound::Decl(undone, held)) => self.restore_decl(undone, held),
        }
    }

    /// rewind で取り消した宣言を戻す。
    fn restore_decl(&mut self, undone: Undone, held: Option<Held>) {
        if self.eng.push_decl(undone.clone()) {
            self.held = held;
            return;
        }
        // 保存しているあいだにエンジンが再起動された。保存した env id は無効に
        // なっているので設定できない。テキストから再実行する。
        if let Some(src) = undone.src {
            self.restore_src(src);
        }
    }

    /// 取り消した宣言を、再起動したエンジンにテキストから戻す。
    ///
    /// 再起動の再実行を打ち切って保留が残っていたら、実行せずに保留の後ろに
    /// 並べる。取り消した宣言は保留の宣言より後ろにあったので、先に実行すると
    /// 順番が変わる。ユーザーが Ctrl-C で再実行を止めたときは、再実行を続ける
    /// ことにもなる。
    fn restore_src(&mut self, src: String) {
        if self.eng.unplayed().is_empty() {
            self.replay_into(&[src]);
            return;
        }
        self.out.write(&self.style.yellow(&format!(
            "再実行しなかった宣言: {}",
            head_line(&src, HEAD_WIDTH)
        )));
        self.out
            .write(&self.style.dim(&format!("  {PENDING_HINT}")));
        self.eng.defer(src);
    }

    // -- 送信 -------------------------------------------------------------

    /// 完結した入力を送って結果を出す。
    fn submit(&mut self, src: &str, kind: Option<Kind>) {
        // 送れるかを確認する前に履歴に追加する。エンジンが異常終了しているときこそ、
        // ユーザーが入力を呼び出せる必要がある。
        self.remember(src);

        if self.eng.env().is_none() {
            // boot に失敗したエンジン。送れば send_cmd のチェックで止まるが、
            // ユーザーに必要なのはエラーの名前ではなく次に何をすればよいかなので、
            // ここで案内を表示する。
            self.out.fail(
                &self
                    .style
                    .red("エンジンが使えない。:restart で再起動できる"),
            );
            return;
        }

        if self.proof.is_some() {
            self.undone = None; // 書き直しが確定した。もう戻さない
            self.tactic(src);
            return;
        }

        let kind = match kind {
            Some(kind) => Some(kind),
            None => self.probe(src).map(|(_, kind)| kind),
        };
        let Some(kind) = kind else {
            // 判定を止めた (Ctrl-C) ので送らない。書き直しも確定していないので、
            // 取り消した宣言を戻す。guard が再起動したので世代が違い、restore_undone は
            // 宣言をテキストから再実行する。
            self.discard();
            self.out
                .write(&self.style.dim("  入力した行は実行しなかった"));
            return;
        };
        self.undone = None; // 書き直しが確定した。もう戻さない

        let t0 = Instant::now();
        self.last = None;
        let sent = match kind {
            Kind::Term => self.submit_term(src),
            Kind::Cmd | Kind::Tac => self.submit_cmd(src),
        };
        if sent {
            self.timing(t0);
        }
    }

    /// 式として `#eval` に包んで送る。環境は進めない。
    fn submit_term(&mut self, src: &str) -> bool {
        let mut is_do = src.trim_start().starts_with("do");
        let mut head = "";
        let mut col: usize = 2;
        let Some(mut resp) = self.send_eval("", col, src) else {
            return false;
        };

        let mut errs = errors(&resp);
        let mut blob = error_blob(&errs);

        // `(← e)` は do の中でしか書けないので、do の外では必ずエラーになる。
        // このエラーのときだけ do で包み直すので、ほかの入力の意味は変わらない。
        // 元の行の相対的なインデントを保つため、`do` は前の行に置く。
        if !errs.is_empty() && !is_do && nested_action(&blob) {
            is_do = true;
            head = " do";
            col = 4;
            let Some(again) = self.send_eval(head, col, src) else {
                return false;
            };
            resp = again;
            errs = errors(&resp);
            blob = error_blob(&errs);
        }

        // `do` を単体で書くと、Lean は最初の action からモナドを決めてしまう
        // (IO.getEnv なら BaseIO)。GHCi と同じく IO として読み直す。
        if !errs.is_empty() && is_do && blob.contains("BaseIO") {
            let retry = format!(
                "#eval show IO _ from{head}\n{}",
                indent_code(src, &" ".repeat(col))
            );
            let again = self.guard(|eng| eng.send_cmd(&retry, false));
            if let Some(again) = again {
                if panic_check(self.out.as_mut(), self.style, &again) {
                    return false;
                }
                if !has_error(&again) {
                    render(self.out.as_mut(), self.style, &again, src, 1, col_off(col));
                    self.last = Some(last_term(src));
                    return true;
                }
            }
        }

        // 評価できない式でも、型だけは表示したほうが親切。
        let reason = if errs.is_empty() {
            None
        } else {
            not_evaluable_reason(&blob)
        };
        if let Some(reason) = reason {
            let check = format!("#check{head}\n{}", indent_code(src, &" ".repeat(col)));
            let out = self.guard(|eng| eng.query(&check)).flatten();
            if let Some(out) = out.filter(|s| !s.is_empty()) {
                self.out.write(out.trim_end());
                self.out
                    .write(&self.style.dim(&format!("-- {reason}。型だけを表示した")));
                self.last = Some(last_term(src));
                return true;
            }
        }

        render(self.out.as_mut(), self.style, &resp, src, 1, col_off(col));
        self.last = Some(last_term(src));
        true
    }

    /// `#eval` に包んで送る。エンジンが使えなくなったか PANIC したら None。
    fn send_eval(&mut self, head: &str, col: usize, src: &str) -> Option<Json> {
        let wrapped = format!("#eval{head}\n{}", indent_code(src, &" ".repeat(col)));
        let resp = self.guard(|eng| eng.send_cmd(&wrapped, false))?;
        if panic_check(self.out.as_mut(), self.style, &resp) {
            None
        } else {
            Some(resp)
        }
    }

    /// command としてそのまま送る。エラーが無ければ環境を 1 つ進める。
    fn submit_cmd(&mut self, src: &str) -> bool {
        let env_before = self.eng.env();
        let Some(resp) = self.guard(|eng| eng.send_cmd(src, false)) else {
            return false;
        };
        if panic_check(self.out.as_mut(), self.style, &resp) {
            return false;
        }

        let advanced = !has_error(&resp) && resp.get("env").is_some();
        if advanced {
            self.eng.accept(&resp, src);
        }
        render(self.out.as_mut(), self.style, &resp, src, 0, 0);

        let found = sorries(&resp);
        self.clear_pending();
        if !found.is_empty() && advanced {
            // エラーになった宣言の sorry は保留しない。sorry を証明で置き換えても
            // 同じエラーで失敗するだけで、置き換えに失敗した直後に「sorry 1 個」
            // と表示してから「sorry 2 個」と表示し直すことになる。
            self.held = Some(Held {
                sorries: found.to_vec(),
                src: src.to_string(),
                env: self.eng.env(),
                at: None,
            });
            self.out.write(&self.style.dim(&format!(
                "-- :prove で証明モードを始められる (sorry {} 個)",
                found.len()
            )));
        }

        self.last = Some(Last {
            src: src.to_string(),
            advanced,
            proof: false,
            env_before,
        });
        true
    }

    /// sorry に関する保留中の状態を捨てる。環境が変わると proofState は無効。
    fn clear_pending(&mut self) {
        self.held = None;
    }

    fn timing(&mut self, t0: Instant) {
        if self.show_time {
            self.out.write(
                &self
                    .style
                    .dim(&format!("({:.2}s)", t0.elapsed().as_secs_f64())),
            );
        }
    }

    // -- 証明モード -------------------------------------------------------

    fn tactic(&mut self, src: &str) {
        // guard の前に取り出しておく。再起動に失敗すると fold_proof も証明モードを
        // 終了するが、取り出しておけば fold_proof は知らせず、ここで 1 回だけ知らせる。
        let Some(proof) = self.proof.take() else {
            return;
        };

        let state = proof.state;
        let resp = self.guard(|eng| eng.send_tactic(&grouped_tactic(src), state));
        if proof.generation != self.eng.generation() {
            // guard がエンジンを再起動した。leani が持つ proofState は前のプロセスの
            // ものなので、応答があっても別の状態を指している。新しいエンジンは番号を
            // 0 から振り直すので、別の証明の状態と番号が一致して「証明完了」と
            // 表示されることもある (宣言は sorry のまま残る)。
            self.drop_proof();
            return;
        }
        self.proof = Some(proof.clone());
        let Some(resp) = resp else {
            return;
        };
        if panic_check(self.out.as_mut(), self.style, &resp) {
            return;
        }

        self.last = Some(last_tactic(src, false));

        if let Some(message) = text(&resp, "message").filter(|m| !m.is_empty()) {
            // エンジンが直接返すエラー。パースエラーなら、括弧で囲まずに読み直したエラーを
            // 表示する。読み直せなければ (probe が None)、エンジンのエラーの位置だけを直す。
            let probe = self.parse(src).flatten();
            let shown = tactic_parse_error(message, probe.as_ref());
            self.out.fail(&self.style.red(shown.trim_end()));
            return;
        }
        if has_error(&resp) {
            render(self.out.as_mut(), self.style, &resp, src, 1, 0);
            return;
        }

        for m in messages(&resp) {
            if let Some(data) = text(m, "data").filter(|d| !d.is_empty()) {
                self.out.write(data.trim_end());
            }
        }

        let Some(next) = int(&resp, "proofState") else {
            render(self.out.as_mut(), self.style, &resp, src, 1, 0);
            return;
        };

        let found = self.suggestion(src, &resp);
        let goals: Vec<String> = list(&resp, "goals")
            .iter()
            .filter_map(Json::as_str)
            .map(String::from)
            .collect();
        let proof = tactic_step(&proof, next, &goals, found.as_deref().unwrap_or(src));
        self.proof = Some(proof.clone());
        self.last = Some(last_tactic(src, true));

        if !goals.is_empty() {
            self.show_goals(Some(&goals));
            return;
        }

        let script = proof.script.join("\n");
        self.out.write(&self.style.green("証明完了。"));
        self.out.write(&self.style.dim("-- スクリプト:"));
        self.out.write(&indent_code(&script, "  "));
        self.proof = None;
        self.last = None;
        self.close_sorry(&script);
    }

    /// `exact?` などの提案 (Try this) をスクリプトに記録する値として取り出す。
    /// 提案が無いか読み取れなければ None (入力したタクティクをそのまま記録する)。
    fn suggestion(&mut self, src: &str, resp: &Json) -> Option<String> {
        let found = try_this(messages(resp))?;
        if !balanced(&found) {
            // 提案を最後まで解析できていない (メッセージの形式が変わった場合など)。
            // 正しくないスクリプトを完成した証明として表示するより、入力したタクティクを
            // そのまま残す。
            self.out.write(&self.style.dim(
                "-- 提案を読み取れなかったので、入力したタクティクをそのままスクリプトに記録した",
            ));
            return None;
        }
        if found != src {
            let shown = found.replace('\n', " ");
            self.out.write(
                &self
                    .style
                    .dim(&format!("-- スクリプトには {shown} を記録した")),
            );
        }
        Some(found)
    }

    /// エンジンが再起動されたか、再起動に失敗したので、証明モードを終了する。
    /// 何が起きたかはユーザーに表示する。
    fn drop_proof(&mut self) {
        self.proof = None;
        self.last = None;
        self.out.write(
            &self
                .style
                .yellow("証明していたエンジンが終了したので、証明モードを終了した"),
        );
        self.out.write(
            &self
                .style
                .dim("  入力したタクティクは証明に反映されていない (宣言は sorry のまま)"),
        );
        match (&self.held, self.eng.env()) {
            (Some(held), Some(_)) => {
                // revive が replay で sorry を取得し直していれば、そのまま証明モードを
                // 再開できる。
                let left = held.sorries.len();
                self.out.write(
                    &self
                        .style
                        .dim(&format!("  :prove で再開できる (sorry {left} 個)")),
                );
            }
            _ => {
                // 再起動に失敗したときの proofState は前のプロセスのもの。
                self.clear_pending();
                self.out
                    .write(&self.style.dim("  :restart で再起動してから入力し直す"));
            }
        }
    }

    /// `:prove` で選んだ sorry をスクリプトで置き換えて、宣言を再実行する。
    fn close_sorry(&mut self, script: &str) {
        let Some(held) = self.held.clone() else {
            return;
        };
        let Some(at) = &held.at else {
            return;
        };

        let Some(new_src) = splice_sorry(&held.src, at, script) else {
            // sorry の位置が分からなかった。証明自体は成功しているので、スクリプトは
            // すでに表示してある。何も表示せずに戻ると「証明完了」という表示だけが残る。
            self.out.write(
                &self
                    .style
                    .dim("-- sorry の位置を読み取れなかったので、宣言は sorry のままにする"),
            );
            return;
        };

        // sorry を含む宣言と名前が同じになるので、先にそれを取り消してから再実行する。
        let undone = if held.env.is_some() && self.eng.env() == held.env {
            Some(self.eng.pop_decl())
        } else {
            None
        };

        // 再実行に失敗したときに戻せるよう保存しておく。sorry が 2 個以上あるときに
        // 保留中の状態を捨てると、残りの sorry を :prove で続けられなくなる。
        self.held = None;

        self.out
            .write(&self.style.dim("-- sorry をスクリプトで置き換えて実行する:"));
        self.out.write(&indent_code(new_src.trim(), "  "));

        let generation = self.eng.generation();
        self.submit(&new_src, Some(Kind::Cmd));
        let landed = self.eng.log().last() == Some(&new_src);
        let Some(undone) = undone else {
            return;
        };
        if landed {
            // 宣言が環境に追加されたかは log の末尾で判断する。世代だけを見ると、
            // 異常終了したエンジンを guard が再起動して再送し、**成功した**ときにも
            // 「sorry のまま」と誤って表示し、sorry を含むテキストを再実行して、
            // 重複エラーになった宣言がずっと保留に残る。
            return;
        }
        if self.eng.generation() != generation {
            // エンジンが再起動され、さらに再実行も失敗した。保存した env id は無効に
            // なっているので設定せず、sorry を含むテキストを再実行する。保留中の状態は
            // 戻さない (replay_into が設定し直した値を無効な proofState で上書きすると、
            // 次の :prove が今の環境に無い状態を指す)。
            self.out.write(
                &self
                    .style
                    .dim("-- エンジンを再起動したので、sorry のままにしておく"),
            );
            if let Some(src) = undone.src {
                self.restore_src(src);
            }
            return;
        }

        // 再実行が失敗した。項の位置にある sorry にはタクティクを差し込めない。
        self.eng.push_decl(undone);
        let left = held.sorries.len();
        self.held = Some(held);
        self.out.write(
            &self
                .style
                .dim("-- エラーになったので、sorry のままにしておく"),
        );
        if left > 1 {
            self.out.write(&self.style.dim(&format!(
                "  残りは :prove <n> で続けられる (sorry {left} 個)"
            )));
        }
    }

    /// ゴールを表示する。None なら証明モードの今のゴール。
    fn show_goals(&mut self, goals: Option<&[String]>) {
        let goals: Vec<String> = match goals {
            Some(gs) => gs.to_vec(),
            None => self
                .proof
                .as_ref()
                .map(|p| p.goals.clone())
                .unwrap_or_default(),
        };

        let total = goals.len();
        for (n, g) in goals.iter().enumerate() {
            let head = if total > 1 {
                format!("goal {}/{total}", n + 1)
            } else {
                "goal".to_string()
            };
            self.out.write(&self.style.green(&head));
            self.out.write(&indent(g, "  "));
        }
    }

    /// 証明モードの外での `:goals`。残っている sorry を出す。
    fn show_pending(&mut self) {
        let Some(held) = &self.held else {
            self.out
                .write(&self.style.dim("証明モードではない (sorry も残っていない)"));
            return;
        };

        let shown: Vec<(String, String)> = held
            .sorries
            .iter()
            .enumerate()
            .map(|(n, sy)| {
                let state = int(sy, "proofState").map_or("None".to_string(), |s| s.to_string());
                let goal = text(sy, "goal").unwrap_or("").trim_end().to_string();
                (format!("sorry {} [proofState {state}]", n + 1), goal)
            })
            .collect();
        for (head, goal) in shown {
            self.out.write(&self.style.green(&head));
            self.out.write(&indent(&goal, "  "));
        }
    }

    /// 直前の入力に出た sorry を 1 つ選んで証明モードを始める。
    fn prove(&mut self, arg: &str) {
        let Some(held) = self.held.clone() else {
            self.out.fail(&self.style.red("直前の入力に sorry が無い"));
            return;
        };
        let found = &held.sorries;

        // 数でなければ 1 番目。0 は 1..n の外になる。
        let n = arg.parse::<usize>().map_or(0, |n| n.wrapping_sub(1));
        let Some(sy) = found.get(n) else {
            let total = found.len();
            self.out.fail(
                &self
                    .style
                    .red(&format!("sorry は {total} 個。1..{total} で指定する")),
            );
            return;
        };

        let goal = text(sy, "goal").unwrap_or("").to_string();
        let state = int(sy, "proofState").unwrap_or(0);
        self.proof = Some(Proof::new(state, vec![goal.clone()], self.eng.generation()));
        // 置き換えるのはこの sorry。位置で特定する
        self.held = Some(Held {
            at: Some(sy.clone()),
            ..held.clone()
        });
        self.out.write(&self.style.dim(PROOF_HELP));
        self.show_goals(Some(&[goal]));
    }

    /// 証明モード専用のコマンド。扱ったら true。
    fn proof_meta(&mut self, cmd: &str) -> bool {
        let Some(proof) = self.proof.clone() else {
            return false;
        };
        match cmd {
            "goals" | "g" => self.show_goals(None),
            "script" => {
                let script = proof.script.join("\n");
                let shown = if script.is_empty() { "(空)" } else { &script };
                self.out.write(&indent_code(shown, "  "));
            }
            "undo" => self.proof_undo(&proof),
            "done" | "q" | "quit" => {
                self.proof = None;
                self.buf.clear();
                self.ready = None;
                self.out.write(&self.style.dim("証明モードを終了した"));
            }
            "?" | "h" | "help" => self.out.write(PROOF_HELP),
            _ => return false,
        }
        true
    }

    fn proof_undo(&mut self, proof: &Proof) {
        let Some(back) = tactic_undo(proof) else {
            self.out
                .write(&self.style.dim("取り消せるタクティクが無い"));
            return;
        };

        self.out
            .write(&self.style.dim(&format!("proofState {}", back.state)));
        self.proof = Some(back);
    }

    // -- コマンド ---------------------------------------------------------

    /// `:` で始まる行を処理する。true を返したらループを抜ける。
    fn meta(&mut self, line: &str) -> bool {
        if let Some(rest) = line.strip_prefix(":!") {
            self.out.shell(rest.trim());
            // 子プロセスは同じプロセスグループにいるので、Ctrl-C の SIGINT は leani にも
            // 届く。止めたいのは子プロセスだけ。フラグをここで消して、ループが入力中の
            // ブロックを捨てないようにする。
            if self.take_interrupt() {
                self.out.write("^C");
            }
            return false;
        }

        let mut parts = line.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("").get(1..).unwrap_or("");
        let arg = parts.next().unwrap_or("").trim().to_string();

        // 証明モード専用のコマンドを先に確認する。該当しなければ通常のコマンドとして
        // 処理する (:t などは証明中でも使える)。
        if self.proof_meta(cmd) {
            return false;
        }

        match cmd {
            "q" | "quit" => return true,
            "?" | "h" | "help" => self.out.put(HELP),
            "t" | "type" => self.cmd_type(&arg),
            "i" | "info" => self.cmd_info(&arg),
            "p" | "print" => self.cmd_print(&arg),
            "loogle" => self.cmd_loogle(&arg),
            "abbrev" => self.cmd_abbrev(&arg),
            "goals" | "g" => self.show_pending(),
            "script" | "done" => self.out.write(&self.style.dim("証明モードではない")),
            "prove" => self.prove(&arg),
            "time" => {
                self.show_time = !self.show_time;
                let state = if self.show_time { "on" } else { "off" };
                self.out
                    .write(&self.style.dim(&format!("実行時間の表示: {state}")));
            }
            "restart" => {
                self.revive();
            }
            "l" | "load" => self.load(&arg, true),
            "r" | "reload" => match self.eng.loaded().map(str::to_string) {
                Some(path) => self.load(&path, true),
                None => self.reset(),
            },
            "reset" => self.reset(),
            "undo" => self.cmd_undo(&arg),
            "save" => self.cmd_save(&arg),
            "env" => self.cmd_env(&arg),
            _ => self
                .out
                .fail(&self.style.red(&format!("不明なコマンド: :{cmd}  (:help)"))),
        }
        // :env で元の環境にも戻れなければ、環境が無いので続けない。
        self.lost.is_some()
    }

    /// 略記の一覧を表示する。引数が記号ならその記号の略記、それ以外なら引数で始まる
    /// 略記。長くなるので pager に出す。
    fn cmd_abbrev(&mut self, arg: &str) {
        let pairs = abbrev_lookup(arg);
        if pairs.is_empty() {
            let what = if arg.is_ascii() {
                "で始まる"
            } else {
                "を入力する"
            };
            self.out
                .write(&self.style.dim(&format!("{arg} {what}略記は無い")));
            return;
        }
        self.out.page(&abbrev_table(&pairs, columns()));
    }

    /// `#check` で式の型を表示する。
    fn cmd_type(&mut self, arg: &str) {
        if arg.is_empty() {
            self.out.fail(&self.style.red(":t には式が必要"));
            return;
        }
        let check = format!("#check\n{}", indent_code(arg, "  "));
        match self.guard(|eng| eng.query(&check)).flatten() {
            Some(out) if !out.is_empty() => self.out.write(out.trim_end()),
            _ => self.out.fail(&self.style.red("型を取得できなかった")),
        }
    }

    /// 名前の型と docstring を表示する。
    fn cmd_info(&mut self, arg: &str) {
        if arg.is_empty() {
            self.out.fail(&self.style.red(":i には名前が必要"));
            return;
        }
        let check = format!("#check @{arg}");
        match self.guard(|eng| eng.query(&check)).flatten() {
            Some(out) if !out.is_empty() => self.out.write(out.trim_end()),
            _ => self.out.fail(&self.style.red(&format!("不明: {arg}"))),
        }
        let query = fill(DOC_QUERY, &[arg]);
        let doc = self.guard(|eng| eng.query(&query)).flatten();
        if let Some(doc) = doc.as_deref().map(str::trim)
            && !doc.is_empty()
        {
            self.out.write(&self.style.dim(doc));
        }
    }

    /// `#print` で定義を表示する。
    fn cmd_print(&mut self, arg: &str) {
        if arg.is_empty() {
            self.out.fail(&self.style.red(":p には名前が必要"));
            return;
        }
        let print = format!("#print {arg}");
        if let Some(resp) = self.guard(|eng| eng.send_cmd(&print, false)) {
            render(self.out.as_mut(), self.style, &resp, arg, 0, 0);
        }
    }

    /// loogle に問い合わせる。エンジンは操作しないので、環境は変わらない。
    fn cmd_loogle(&mut self, arg: &str) {
        if arg.is_empty() {
            self.out.fail(
                &self
                    .style
                    .red(":loogle には名前か型のパターンが必要  (例: |- ?a + ?b = ?b + ?a)"),
            );
            return;
        }
        match self.ask_loogle(arg) {
            Ok(got) => self
                .out
                .write(&loogle_text(&got, 10, columns(), self.style)),
            Err(e) => self
                .out
                .fail(&self.style.red(&format!("loogle の検索に失敗した: {e}"))),
        }
    }

    /// loogle に問い合わせているあいだ、メッセージを 1 行表示する。
    ///
    /// 重いパターンだと loogle の処理に 20 秒近くかかる。何も表示しないと leani が
    /// 止まったように見える。成功しても失敗しても、次の出力を表示する前にこの行を消す。
    fn ask_loogle(&mut self, query: &str) -> Result<Json, leani_core::types::SearchError> {
        if !self.ctx.tty {
            return self.search.loogle(query);
        }
        self.out.put(&self.style.dim("loogle に問い合わせている…"));
        let got = self.search.loogle(query);
        self.out.put("\r\x1b[K");
        got
    }

    /// 引数が無ければ今の環境を表示し、名前を渡すとその環境で再起動する。
    fn cmd_env(&mut self, arg: &str) {
        if arg.is_empty() {
            self.show_env();
            return;
        }
        let found = load_config(&self.ctx.config)
            .and_then(|conf| resolve(&self.ctx, Some(arg), None, &[], &conf));
        let target = match found {
            Ok(target) => target,
            Err(_) if arg == self.eng.cfg().name => {
                // -p から推測した環境のように、設定に無い環境で起動したとき。
                self.out.write(&self.style.dim(&format!("すでに {arg}")));
                return;
            }
            Err(e) => {
                self.out.fail(&self.style.red(&e.to_string()));
                return;
            }
        };
        // 名前ではなく設定全体で比べる。-i を付けて起動した環境と、-p から推測した
        // 環境は、設定にある同じ名前の環境と import やプロジェクトが違う。
        if target == *self.eng.cfg() {
            self.out.write(&self.style.dim(&format!("すでに {arg}")));
            return;
        }

        // 今のエンジンを終了させる前に準備を済ませる。
        let why = match problem(&self.ctx, &target) {
            Some(why) => Some(why),
            None => prepare(&self.ctx, &mut ToOutput(self.out.as_mut()), &target)
                .err()
                .map(|e| e.to_string()),
        };
        if let Some(why) = why {
            self.out.fail(&self.style.red(&why)); // 今のエンジンは終了させない
            return;
        }

        let keep = self.show_time;
        let prev = self.eng.loaded().map(str::to_string);
        let back = self.eng.cfg().clone();
        // 入力した宣言も新しいエンジンに引き継ぐ。:l したファイルの内容は preload で
        // 戻るが、対話で入力した宣言は新しい Engine には含まれていない。起動後に
        // 再実行する。
        // 保留中の宣言も引き継ぐ。捨てると、直前に「保留にした」と表示した宣言が
        // :env で何も表示されずに消える。新しい環境ならエラーなく実行
        // できることもある (import が増える方向の切り替え)。失敗したらまた保留に戻る。
        let mut log = self.eng.log().to_vec();
        log.extend_from_slice(self.eng.unplayed());
        let mut hook = self.eng.take_hook();
        self.eng.kill();

        let eng = match boot_engine(&self.ctx, target, &mut hook, self.out.as_mut(), self.style) {
            Ok(eng) => eng,
            Err((e, dead)) => {
                // import が成功するかは boot するまで分からない。元の環境に戻す。
                self.keep_failed(dead);
                self.out.fail(
                    &self
                        .style
                        .red(&format!("{arg} で起動できなかった: {}", reason(&e))),
                );
                self.out
                    .write(&self.style.dim(&format!("  {} に戻る", back.name)));
                let name = back.name.clone();
                match boot_engine(&self.ctx, back, &mut hook, self.out.as_mut(), self.style) {
                    Ok(eng) => eng,
                    Err((EngineFault::Interrupted, dead)) => {
                        // 戻るのを Ctrl-C で中断した。環境は無いが理由も無いので、
                        // :restart の中断と同じく、宣言を保留にしてセッションを続ける。
                        // 古いエンジンを残す。:l したファイルと init を持っているのは
                        // 古いエンジンなので、:restart がそれを読み込み直せる。
                        self.out.write(&self.style.yellow(&format!(
                            "^C {name} に戻るのを中断した。:restart でやり直せる"
                        )));
                        self.keep_failed(dead);
                        self.lose_engine(None, hook);
                        self.eng.carry(log);
                        return;
                    }
                    Err((e, dead)) => {
                        // 環境が無いまま続けても、どの入力もエラーになるだけ。端末は
                        // 終了し、カーネルは次のセルでエンジンを起動し直す。
                        self.lost = Some(format!("{name} にも戻れなくなった: {e}"));
                        self.lose_engine(dead, hook);
                        return;
                    }
                }
            }
        };
        self.install(eng, prev.as_deref());

        // 切り替えが成功しても元の環境に戻っても、対話で入力した宣言は新しい
        // エンジンには無い。どちらの場合も、ここで 1 回だけ再実行する。
        self.replay_into(&log);
        self.show_time = keep;
    }

    /// `:env` で元の環境にも戻れなかったあとの後始末。
    ///
    /// `dead` があれば、終了させた古いエンジンを、環境の無い `dead` に入れ替える。
    /// 古いエンジンは宣言の記録を持ったままなので、入れ替えて状態のつじつまを合わせる。
    /// `dead` が無ければ古いエンジンを残す。差し込み口は残ったエンジンに戻す。
    fn lose_engine(&mut self, dead: Option<Box<Engine>>, hook: Option<ExchangeHook>) {
        if let Some(dead) = dead {
            self.eng = *dead;
        }
        self.eng.set_hook(hook);
        self.forget();
    }

    /// boot に失敗して終了させたエンジンを、テストが確かめられるように残す。
    #[cfg(feature = "testing")]
    fn keep_failed(&mut self, dead: Option<Box<Engine>>) {
        self.failed.extend(dead.map(|b| *b));
    }

    #[cfg(not(feature = "testing"))]
    fn keep_failed(&mut self, _dead: Option<Box<Engine>>) {}

    /// 今の環境と、設定にある切り替え先を表示する。
    fn show_env(&mut self) {
        let cfg = self.eng.cfg();
        let project = cfg.project.as_deref().unwrap_or("Lake プロジェクト無し");
        self.out
            .write(&self.style.dim(&format!("{cfg}: {project}")));
        self.out.write(&self.style.dim(&format!(
            "env {} (base {}, 宣言 {} 件)",
            env_text(self.eng.env()),
            env_text(self.eng.base()),
            self.eng.log().len()
        )));
        let names =
            load_config(&self.ctx.config).and_then(|conf| env_names(&conf, &self.ctx.config));
        match names {
            Ok(names) if names.is_empty() => {}
            Ok(names) => self.out.write(
                &self
                    .style
                    .dim(&format!("切り替え先: {}  (:env <name>)", names.join(" "))),
            ),
            Err(e) => self.out.fail(&self.style.red(&e.to_string())),
        }
    }

    /// 直前の n 件の宣言を取り消す。
    fn cmd_undo(&mut self, arg: &str) {
        for _ in 0..arg.parse::<usize>().unwrap_or(1) {
            self.eng.pop_decl();
        }
        self.clear_pending();
        self.out
            .write(&self.style.dim(&format!("env {}", env_text(self.eng.env()))));
    }

    /// 実行した宣言を .lean ファイルとして書き出す。
    ///
    /// repl には環境を pickle する機能もあるが、unpickle した環境で #eval すると
    /// Lean のコンパイラが PANIC する (コンパイラの状態が pickle に含まれない)。
    /// ソースで保存しておけば編集もできるし、lean でそのまま実行できる。
    fn cmd_save(&mut self, arg: &str) {
        let Some(path) = abspath(arg, &self.ctx.home, &self.ctx.cwd) else {
            self.out.fail(&self.style.red(":save にはファイル名が必要"));
            return;
        };

        let srcs = self.eng.sources();
        let orphans = self.eng.unplayed().to_vec();
        if srcs.is_empty() && orphans.is_empty() {
            self.out.fail(&self.style.red("保存する宣言が無い"));
            return;
        }

        if std::fs::symlink_metadata(&path).is_ok() && !self.saved.contains(&path) {
            // パスの入力を間違えて、プロジェクトのソースを上書きしない。同じパスへの
            // 2 回目以降の :save は上書きする。`exists` はリンク先を見るので、リンク先の
            // 無いシンボリックリンクを「無い」と判定してしまう。
            self.out
                .fail(&self.style.red(&format!("すでにある: {path}")));
            self.out
                .write(&self.style.dim("  ファイルを消すか、別の名前を指定する"));
            return;
        }

        // 起動時と同じヘッダを書く。設定の import だけだと lean で直接実行したときに
        // エラーになる (leani は起動時と :l のときに import Lean を自分で追加する)。
        // :l したファイルの import も追加する (save_header)。
        let body = save_body(&srcs, &orphans);
        let file = format!("{}\n{body}\n", self.eng.save_header());
        if let Err(e) = std::fs::write(&path, file) {
            self.out.fail(
                &self
                    .style
                    .red(&format!("書き出せなかった: {}", strerror(&e))),
            );
            return;
        }

        self.saved.insert(path.clone());
        self.out.write(
            &self
                .style
                .dim(&format!("宣言 {} 件を書き出した: {path}", srcs.len())),
        );
        self.out.write(&self.style.dim(&format!("-- :l {path}")));
    }

    /// ファイルを読み込んで環境を作り直す。`announce` が偽なら成功の表示を出さない。
    pub fn load(&mut self, path: &str, announce: bool) {
        if path.is_empty() {
            self.out.fail(&self.style.red("ファイル名が必要"));
            return;
        }

        let path = expand_home(path, &self.ctx.home);
        if !Path::new(&path).is_file() {
            self.out
                .fail(&self.style.red(&format!("ファイルが無い: {path}")));
            return;
        }

        let src = match std::fs::read_to_string(&path) {
            Ok(src) => src,
            Err(e) => {
                let why = strerror(&e);
                self.out
                    .fail(&self.style.red(&format!("読めない: {path} ({why})")));
                return;
            }
        };

        // 読み込みが成功すると保留は捨てられる。
        let left = self.eng.unplayed().len();
        // base 環境が変わるので、補完のキャッシュは使えなくなる。
        self.comp_cache.clear();
        let Some(out) = self.guard(|eng| eng.load_file(&path, Some(&src))) else {
            return;
        };

        render(self.out.as_mut(), self.style, &out.resp, &out.src, 0, 0);
        if let Some(note) = &out.note {
            self.out.write(&self.style.yellow(note));
        }
        if out.bad {
            self.out
                .fail(&self.style.red(&format!("読み込めなかった: {path}")));
            return;
        }

        // 環境がすべて置き換わるので、前の環境の proofState は使えない。
        self.proof = None;
        self.last = None;
        self.undone = None;
        self.clear_pending();
        if announce {
            self.out.write(&self.style.dim(&format!(
                "読み込んだ: {path} (env {})",
                env_text(self.eng.env())
            )));
        }
        if left > 0 {
            // :reset と同じく、何も表示せずに捨てるとユーザーが気付けない。
            self.out
                .write(&self.style.dim(&format!("  保留中の宣言 {left} 件も捨てた")));
        }
    }

    /// 宣言をすべて取り消して、起点の環境に戻す。
    fn reset(&mut self) {
        // 保留も捨てる。残すと、:reset で消したはずの宣言が次の :restart で戻ってくる。
        let left = match self.eng.reset() {
            Ok(left) => left,
            Err(_) => {
                // 起点の環境が無い (boot に失敗した)。env を設定し直しても、どの入力にも
                // "Unknown environment." しか返さない状態になり、保留中の宣言だけが消える。
                self.out.fail(
                    &self
                        .style
                        .red("元になる環境が無い。:restart で再起動できる"),
                );
                return;
            }
        };
        self.proof = None;
        self.last = None;
        self.clear_pending();
        self.out.write(
            &self
                .style
                .dim(&format!("env {} に戻した", env_text(self.eng.env()))),
        );
        if left > 0 {
            self.out
                .write(&self.style.dim(&format!("  保留中の宣言 {left} 件も捨てた")));
        }
    }
}

/// エンジンを作って起点の環境を作る。起動時と `:env` の切り替えで使う。
///
/// `hook` はテストの差し込み口で、作った `Engine` に移す。boot に失敗したら `hook`
/// に戻し、プロセスを終了させた `Engine` も返す。Engine を作った時点で repl の
/// プロセスは起動しているので、呼び出し元が捨てる前に終了させておく (Drop も kill
/// するが、意図を書いておく)。
fn boot_engine(
    ctx: &Ctx,
    cfg: EnvConfig,
    hook: &mut Option<ExchangeHook>,
    out: &mut dyn Output,
    style: Style,
) -> Result<Engine, (EngineFault, Option<Box<Engine>>)> {
    let mut eng = match Engine::new(ctx, cfg, &mut ToOutput(out)) {
        Ok(eng) => eng,
        Err(e) => return Err((e, None)),
    };
    eng.set_hook(hook.take());
    if let Some(warning) = eng.warning() {
        out.write(&style.yellow(warning));
    }
    let t0 = Instant::now();
    if let Err(e) = eng.boot() {
        *hook = eng.take_hook();
        eng.kill();
        return Err((e, Some(Box::new(eng))));
    }
    out.write(&style.dim(&format!(
        "leani: {} / {} / {:.1}s",
        eng.toolchain(),
        eng.cfg(),
        t0.elapsed().as_secs_f64()
    )));
    Ok(eng)
}

/// 失敗の理由。空なら「理由は分からない」。
fn reason(e: &EngineFault) -> String {
    match e.to_string() {
        s if s.is_empty() => "理由は分からない".to_string(),
        s => s,
    }
}

/// 標準入力と標準出力が端末のときだけ履歴を持つ。履歴のディレクトリが無ければ作る。
/// 作れなくても履歴が残らないだけなので、エラーは無視する。
fn history_for(ctx: &Ctx) -> Option<BlockHistory> {
    if !ctx.tty || !ctx.stdin_tty {
        return None;
    }
    if let Some(dir) = Path::new(&ctx.hist).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    Some(BlockHistory::new(&ctx.hist))
}

/// 報告に使う宣言の先頭行の幅。
const HEAD_WIDTH: usize = 60;

/// panic のメッセージ。`panic!` の引数は `&str` か `String` で届く。
fn panic_text(payload: &(dyn Any + Send)) -> String {
    match (
        payload.downcast_ref::<&str>(),
        payload.downcast_ref::<String>(),
    ) {
        (Some(s), _) => (*s).to_string(),
        (_, Some(s)) => s.clone(),
        _ => "panic".to_string(),
    }
}

/// JSON の配列のうち文字列だけを取り出す。配列でなければ空。
fn list_of_strings(v: &Json) -> Vec<String> {
    v.as_array().map_or_else(Vec::new, |xs| {
        xs.iter()
            .filter_map(Json::as_str)
            .map(String::from)
            .collect()
    })
}

/// `env {n}` の表示。環境が無ければ `None`。
fn env_text(env: Option<i64>) -> String {
    match env {
        Some(n) => n.to_string(),
        None => "None".to_string(),
    }
}

/// エラーのメッセージをつないだもの。評価できない理由の判定に使う。
fn error_blob(errs: &[&Json]) -> String {
    errs.iter()
        .map(|m| text(m, "data").unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `#eval` に包んだ分の桁のずれ。
fn col_off(col: usize) -> i64 {
    i64::try_from(col).unwrap_or(0)
}

/// 式の評価は環境を進めない。
fn last_term(src: &str) -> Last {
    Last {
        src: src.to_string(),
        advanced: false,
        proof: false,
        env_before: None,
    }
}

/// 証明モードのタクティク。`advanced` は proofState を進めたか (取り消すべきか)。
fn last_tactic(src: &str, advanced: bool) -> Last {
    Last {
        src: src.to_string(),
        advanced,
        proof: true,
        env_before: None,
    }
}
