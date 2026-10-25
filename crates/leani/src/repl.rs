//! 入力を 1 行ずつ受け取り、エンジンに送って結果を表示する (副作用)。
//!
//! Python 版の `repl.py` から移している途中。この段階では、式と宣言の評価、
//! `:{ ... :}`、エンジンの再起動、`:help` `:time` `:restart` `:quit` だけがある。
//! 証明モード、`:l`、`:env`、補完、履歴はあとの段階で足す。
//!
//! 入力を読むループは `Repl` の外に置く (計画の「テスト用の差し込み口」)。`Repl` は
//! `feed` で 1 行ずつ受け取るだけなので、端末、パイプ、Jupyter、テストのどれからでも
//! 同じように呼べる。

use std::io;
use std::path::Path;
use std::time::Instant;

use leani_core::pure::{
    Style, block_continues, classify, classify_tac, continues, errors, has_error, head_line,
    indent, last_json, lean_str, nested_action, not_evaluable_reason, sorries, strip_imports, text,
};
use leani_core::queries::{META_LINE, PARSE_PROBE, fill};
use leani_core::types::{EngineFault, Json, Kind, Output, Proof, State};
use leani_read::config::EnvConfig;
use leani_read::places::Ctx;

use crate::engine::{Engine, Replay, Undone, strerror};
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

    // 証明モード
    proof: Option<Proof>,
    held: Option<Held>,
}

impl Repl {
    /// エンジンを起動して起点の環境を作る。表示はすべて `out` に出す。
    pub fn new(ctx: &Ctx, cfg: EnvConfig, mut out: Box<dyn Output>) -> Result<Repl, EngineFault> {
        let style = Style { color: ctx.color };
        let mut eng = Engine::new(ctx, cfg, &mut ToOutput(out.as_mut()))?;
        if let Some(warning) = eng.warning() {
            out.write(&style.yellow(warning));
        }
        let t0 = Instant::now();
        if let Err(e) = eng.boot() {
            // Engine を作った時点で repl のプロセスは起動している。ここで返すと呼び出し元が
            // Engine を捨てるので、そのプロセスを終了させておく (Drop も kill するが、
            // 意図を書いておく)。
            eng.kill();
            return Err(e);
        }
        out.write(&style.dim(&format!(
            "leani: {} / {} / {:.1}s",
            eng.toolchain(),
            eng.cfg(),
            t0.elapsed().as_secs_f64()
        )));

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
            proof: None,
            held: None,
        };
        if Path::new(&repl.ctx.init).is_file() {
            repl.apply_init();
        }
        Ok(repl)
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
    fn apply_init(&mut self) {
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
    ///
    /// 履歴は段階 5b で足す。それまでは何もしない。呼び出し箇所は Python 版と同じに
    /// しておく。
    fn remember(&mut self, _src: &str) {}

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
                let why = match e.to_string() {
                    s if s.is_empty() => "理由は分からない".to_string(),
                    s => s,
                };
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
        // 証明モードの終了の表示 (drop_proof) は段階 5c で足す。
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
    fn replay_into(&mut self, log: &[String]) {
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
    pub fn feed_line(&mut self, line: &str) -> bool {
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
            // タクティクの取り消しは段階 5c で足す。
            return;
        }
        // 書き直しをやめたときに戻せるよう保存しておく
        // (Ctrl-C / Ctrl-D / ブロックの入力をやめたとき)。取り消した宣言の
        // sorry の保留も、今の環境のものではなくなるので一緒に保存する。
        let undone = self.eng.pop_decl();
        self.undone = Some((undone, self.held.take()));
    }

    /// rewind で取り消した宣言を戻す。書き直さずにやめたとき。
    fn restore_undone(&mut self) {
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
        // 証明モードのタクティク (tactic) は段階 5c で足す。

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

    // -- コマンド ---------------------------------------------------------

    /// `:` で始まる行を処理する。true を返したらループを抜ける。
    ///
    /// この段階にあるのは `:quit` `:help` `:time` `:restart` だけ。ほかのコマンドは
    /// あとの段階で足す。それまでは「不明なコマンド」になる。
    fn meta(&mut self, line: &str) -> bool {
        let mut parts = line.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("").get(1..).unwrap_or("");

        match cmd {
            "q" | "quit" => return true,
            "?" | "h" | "help" => self.out.put(HELP),
            "script" | "done" => self.out.write(&self.style.dim("証明モードではない")),
            "time" => {
                self.show_time = !self.show_time;
                let state = if self.show_time { "on" } else { "off" };
                self.out
                    .write(&self.style.dim(&format!("実行時間の表示: {state}")));
            }
            "restart" => {
                self.revive();
            }
            _ => self
                .out
                .fail(&self.style.red(&format!("不明なコマンド: :{cmd}  (:help)"))),
        }
        false
    }
}

/// 報告に使う宣言の先頭行の幅 (Python 版の `head_line` の既定値)。
const HEAD_WIDTH: usize = 60;

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
