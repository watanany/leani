//! 入力を 1 行ずつ受け取り、エンジンに送って結果を表示する (副作用)。
//!
//! Python 版の `repl.py` から移している途中。この段階では、式と宣言の評価、
//! `:{ ... :}`、エンジンの再起動、init、`:l` `:r` `:reset` `:undo` `:save`、履歴、
//! `:help` `:time` `:restart` `:quit` `:!`、証明モード、`:env` がある。補完は
//! あとの段階で足す。
//!
//! 入力を読むループは `Repl` の外に置く (計画の「テスト用の差し込み口」)。`Repl` は
//! `feed` で 1 行ずつ受け取るだけなので、端末、パイプ、Jupyter、テストのどれからでも
//! 同じように呼べる。

use std::any::Any;
use std::collections::HashSet;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::time::Instant;

use leani_core::pure::{
    Style, abspath, balanced, block_continues, classify, classify_tac, continues, errors,
    has_error, head_line, indent, int, last_json, lean_str, list, messages, nested_action,
    not_evaluable_reason, save_body, sorries, splice_sorry, strip_imports, tactic_step,
    tactic_undo, text, try_this,
};
use leani_core::queries::{META_LINE, PARSE_PROBE, fill};
use leani_core::types::{EngineFault, Json, Kind, Output, Proof, State};
use leani_read::config::{EnvConfig, env_names, load_config, problem, resolve};
use leani_read::places::Ctx;

use crate::boot::prepare;
use crate::engine::{Engine, ExchangeHook, Replay, Undone, strerror};
use crate::history::BlockHistory;
use crate::show::{panic_check, render};

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

/// 1 つのセッション。
///
/// セッションの状態 (入力バッファ、直前の入力、証明モード、保留した sorry) を持つのは
/// `Engine` とこの struct だけ。`Engine` の状態は `Engine` のメソッドだけが書き換え、
/// `Repl` は読むためのメソッドだけを使う。
pub struct Repl {
    ctx: Ctx,
    out: Box<dyn Output>,
    style: Style,
    eng: Engine,

    // 入力バッファ
    buf: Vec<String>,
    /// 構文的に完結しているときの送り方。
    ready: Option<Kind>,
    last: Option<Last>,
    /// `:{ ... :}` の中か。
    explicit: bool,
    /// rewind で取り消した宣言と、その宣言の sorry の保留。
    undone: Option<(Undone, Option<Held>)>,
    show_time: bool,
    /// このセッションで `:save` したパス。同じパスへの 2 回目以降は上書きする。
    saved: HashSet<String>,
    /// 標準入力が端末のときだけ履歴を残す。パイプからの入力は履歴にしない。
    history: Option<BlockHistory>,

    // 証明モード
    proof: Option<Proof>,
    held: Option<Held>,

    /// `:env` で元の環境にも戻れなくなった理由。環境が無いまま続けても、どの入力も
    /// エラーになるだけなので、`feed` はループを抜けるよう true を返す。端末はこの
    /// 理由を表示して終了する (Python 版の `EnvLost`)。
    lost: Option<String>,
    /// `:env` の切り替えで boot に失敗して終了させたエンジン。プロセスを残して
    /// いないことをテストが確かめる。
    #[cfg(feature = "testing")]
    failed: Vec<Engine>,
}

impl Repl {
    /// エンジンを起動して起点の環境を作る。表示はすべて `out` に出す。
    /// `preload` は起動時に `:l` するファイル (`leani file.lean`)。
    pub fn new(
        ctx: &Ctx,
        cfg: EnvConfig,
        preload: Option<&str>,
        mut out: Box<dyn Output>,
    ) -> Result<Repl, EngineFault> {
        let style = Style { color: ctx.color };
        let eng = boot_engine(ctx, cfg, &mut None, out.as_mut(), style).map_err(|(e, _)| e)?;

        let mut repl = Repl {
            ctx: ctx.clone(),
            out,
            style,
            eng,
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

    /// 新しいエンジンに入れ替えて、セッションの状態を起動した直後に戻す
    /// (Python 版の `_start` の後半)。
    fn install(&mut self, eng: Engine, preload: Option<&str>) {
        self.eng = eng;
        self.forget();
        self.begin(preload);
    }

    /// セッションの状態を起動した直後に戻す。
    fn forget(&mut self) {
        self.buf.clear();
        self.ready = None;
        self.last = None;
        self.explicit = false;
        self.undone = None;
        self.show_time = false;
        self.proof = None;
        self.held = None;
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

    /// init ファイルのパスを差し替える。テストだけが使う (Python 版の `INIT` の
    /// monkeypatch に当たる)。
    #[cfg(feature = "testing")]
    #[doc(hidden)]
    pub fn set_init(&mut self, path: &str) {
        self.ctx.init = path.to_string();
    }

    /// rewind で取り消した宣言を直接置く。テストだけが使う。
    #[cfg(feature = "testing")]
    #[doc(hidden)]
    pub fn set_undone(&mut self, undone: Undone) {
        self.undone = Some((undone, None));
    }

    /// 次の入力のプロンプト。証明モードでは `⊢> `。
    pub fn prompt(&self) -> String {
        match self.proof {
            Some(_) => self.style.c("35", "⊢> "),
            None => self.style.c("36", &self.eng.cfg().prompt),
        }
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

        let Some(resp) = self.guard(|eng| eng.run_init(&src)) else {
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

    /// エンジンを再起動して宣言を replay する。再起動できなければ None を返す。
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
        self.report_replay(&out);
        self.reattach(&out);
        Some(out)
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

        let Some(out) = self.guard(|eng| Ok(eng.replay(log))) else {
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
    /// Ctrl-C で止めた場合は再実行しない (ユーザーは止めたいので)。
    ///
    /// 再実行でもエンジンが終了したら、もう一度だけ再起動して、再実行はしない。
    /// 同じ入力でエンジンが毎回異常終了する場合がある。再起動しないと、終了した
    /// プロセスが残り、次の入力もすべて失敗する。
    ///
    /// それ以外のエラー (`NoEnvironment`、`Io`、`Setup`) は Python 版では例外のまま
    /// ループまで上がり、「内部エラー」として表示していた。ここでは同じ表示を出して
    /// None を返す。
    fn guard<T>(&mut self, f: impl Fn(&mut Engine) -> Result<T, EngineFault>) -> Option<T> {
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

        self.revive()?;

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

    /// src が command / term / tacticSeq として読めるかを Lean のパーサに問い合わせる。
    /// 1 回のやりとりで 3 つとも取得する。実行はしない。ユーザー定義の notation も
    /// 認識される。
    fn parse(&mut self, src: &str) -> Option<Json> {
        let probe = fill(PARSE_PROBE, &[&lean_str(src)]);
        let got = self.guard(|eng| eng.query(&probe))?;
        last_json(got.as_deref()).filter(Json::is_object)
    }

    fn probe(&mut self, src: &str) -> (State, Kind) {
        classify(self.parse(src).as_ref())
    }

    fn probe_tac(&mut self, src: &str) -> (State, Kind) {
        classify_tac(self.parse(src).as_ref())
    }

    // -- ループから呼ぶもの -----------------------------------------------

    /// 端末から受け取った 1 件を処理する。履歴から呼び出した入力は改行を含む。
    /// true を返したらループを抜ける。
    ///
    /// 想定外の panic でも、その 1 件のエラーとして扱ってセッションを続ける。
    /// セッションを終了するとそれまでの宣言をすべて失うので、それが一番大きな
    /// 損失になる。リクエストを書いてから応答を読むまでのあいだに panic すると、
    /// 読んでいない応答が残って次の応答とずれるので、エンジンを再起動して宣言を
    /// replay する。Python 版はここで再起動しなかったが、Python 版の例外は
    /// パイプから読む途中では起きにくかった。
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

    /// 入力中のブロックを捨てる。Ctrl-C / Ctrl-D で呼ぶ。
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

    /// ノートブックのセルの始まり。前のセルの宣言に、このセルのインデント行を
    /// 続けないようにする。
    pub fn start_block(&mut self) {
        self.last = None;
    }

    /// ノートブックのセルの終わりで入力を確定する。端末で空行を入力したときと同じ。
    /// `:{` が閉じていなければ、`:{` から後ろを捨てて、そのことを表示する。
    pub fn end_block(&mut self) {
        if self.explicit {
            self.out
                .fail(&self.style.red(":} が無いので、:{ から後ろは実行しなかった"));
            self.discard();
        } else if !self.buf.is_empty() {
            self.feed("");
        }
    }

    /// エンジンを終了させる。
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
        let (state, kind) = if proving {
            self.probe_tac(&src)
        } else {
            self.probe(&src)
        };
        if proving && self.proof.is_none() {
            // probe の最中に guard がエンジンの異常終了を検出し、revive が証明モードを
            // 終了した。このまま submit すると proof が None なので command として
            // 処理され、タクティクの行が宣言として送られて
            // "unexpected identifier; expected command" になる。さらに
            // submit_cmd の clear_pending が reattach で設定した値を消すので、直前に
            // 表示した「:prove で再開できる」も正しくなくなる。入力した行は送らずに
            // 捨て、案内だけを表示する。
            self.remember(&src);
            self.buf.clear();
            self.ready = None;
            self.out
                .write(&self.style.dim("  入力した行は実行しなかった"));
        } else if state == State::More {
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
        if last.proof {
            // 証明モードのタクティクは環境を進めていない。証明モードが終了していても、
            // ここで宣言を pop してはいけない (直前の実際の宣言が消える)。
            if let Some(proof) = &self.proof {
                self.proof = tactic_undo(proof).or_else(|| Some(proof.clone()));
            }
            return;
        }
        // 書き直しをやめたときに戻せるよう保存しておく
        // (Ctrl-C / Ctrl-D / ブロックの入力をやめたとき)。取り消した宣言の
        // sorry の保留も、今の環境のものではなくなるので一緒に保存する。
        let undone = self.eng.pop_decl();
        self.undone = Some((undone, self.held.take()));
    }

    /// rewind で取り消した宣言を戻す。書き直さずにやめたとき。
    pub fn restore_undone(&mut self) {
        let Some((undone, held)) = self.undone.take() else {
            return;
        };
        if self.eng.push_decl(undone.clone()) {
            self.held = held;
            return;
        }
        if let Some(src) = undone.src {
            // 保存しているあいだにエンジンが再起動された。保存した env id は無効に
            // なっているので設定できない。テキストから再実行する。
            self.replay_into(&[src]);
        }
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

        self.undone = None; // 書き直しが確定した。もう戻さない
        if self.proof.is_some() {
            self.tactic(src);
            return;
        }

        let kind = match kind {
            Some(kind) => kind,
            None => self.probe(src).1,
        };

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
                indent(src, &" ".repeat(col))
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
            let check = format!("#check{head}\n{}", indent(src, &" ".repeat(col)));
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
        let wrapped = format!("#eval{head}\n{}", indent(src, &" ".repeat(col)));
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

    /// 1 行 = 1 タクティク。proofState を進める。
    fn tactic(&mut self, src: &str) {
        let Some(proof) = self.proof.clone() else {
            return;
        };

        let state = proof.state;
        let resp = self.guard(|eng| eng.send_tactic(src, state));
        if self.proof.as_ref() != Some(&proof) || proof.generation != self.eng.generation() {
            // guard がエンジンを再起動した。leani が持つ proofState は前のプロセスの
            // ものなので、応答があっても別の状態を指している。新しいエンジンは番号を
            // 0 から振り直すので、別の証明の状態と番号が一致して「証明完了」と
            // 表示されることもある (宣言は sorry のまま残る)。
            self.drop_proof();
            return;
        }
        let Some(resp) = resp else {
            return;
        };
        if panic_check(self.out.as_mut(), self.style, &resp) {
            return;
        }

        self.last = Some(last_tactic(src, false));

        if let Some(message) = text(&resp, "message").filter(|m| !m.is_empty()) {
            // エンジンが直接返すエラー
            self.out.fail(&self.style.red(message.trim_end()));
            return;
        }
        if has_error(&resp) {
            render(self.out.as_mut(), self.style, &resp, src, 0, 0);
            return;
        }

        for m in messages(&resp) {
            if let Some(data) = text(m, "data").filter(|d| !d.is_empty()) {
                self.out.write(data.trim_end());
            }
        }

        let Some(next) = int(&resp, "proofState") else {
            render(self.out.as_mut(), self.style, &resp, src, 0, 0);
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
        self.out.write(&indent(&script, "  "));
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
        self.out.write(&indent(new_src.trim(), "  "));

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
                self.replay_into(&[src]);
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
                self.out.write(&indent(shown, "  "));
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
    ///
    /// `:t` `:i` `:p` `:loogle` `:abbrev` はあとの段階で足す。それまでは
    /// 「不明なコマンド」になる。
    fn meta(&mut self, line: &str) -> bool {
        if let Some(rest) = line.strip_prefix(":!") {
            self.out.shell(rest.trim());
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

    /// 引数が無ければ今の環境を表示し、名前を渡すとその環境で再起動する。
    fn cmd_env(&mut self, arg: &str) {
        if arg.is_empty() {
            self.show_env();
            return;
        }
        if arg == self.eng.cfg().name {
            self.out.write(&self.style.dim(&format!("すでに {arg}")));
            return;
        }

        let found = load_config(&self.ctx.config)
            .and_then(|conf| resolve(&self.ctx, Some(arg), None, &[], &conf));
        let target = match found {
            Ok(target) => target,
            Err(e) => {
                self.out.fail(&self.style.red(&e.to_string()));
                return;
            }
        };

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
                    Err((e, dead)) => {
                        // 環境が無いまま続けても、どの入力もエラーになるだけ。端末は
                        // 終了し、カーネルは次のセルでエンジンを起動し直す。
                        // 終了させた古いエンジンは宣言の記録を持ったままなので、環境の
                        // 無いエンジンに入れ替えて、状態のつじつまを合わせる。
                        self.lost = Some(format!("{name} にも戻れなくなった: {e}"));
                        if let Some(dead) = dead {
                            self.eng = *dead;
                        }
                        self.forget();
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

        if Path::new(&path).exists() && !self.saved.contains(&path) {
            // パスの入力を間違えて、プロジェクトのソースを上書きしない。同じパスへの
            // 2 回目以降の :save は上書きする。
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

/// 標準入力が端末のときだけ履歴を持つ。履歴のディレクトリが無ければ作る。作れなくても
/// 履歴が残らないだけなので、エラーは無視する。
fn history_for(ctx: &Ctx) -> Option<BlockHistory> {
    if !ctx.tty {
        return None;
    }
    if let Some(dir) = Path::new(&ctx.hist).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    Some(BlockHistory::new(&ctx.hist))
}

/// `~` だけを展開する (Python の `os.path.expanduser`)。相対パスはそのまま残す。
fn expand_home(path: &str, home: &str) -> String {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{home}{rest}"),
        _ => path.to_string(),
    }
}

/// 報告に使う宣言の先頭行の幅 (Python 版の `head_line` の既定値)。
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

/// `env {n}` の表示。環境が無ければ Python 版と同じく `None`。
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
