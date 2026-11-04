//! エンジン (副作用)。
//!
//! repl のプロセスを 1 つ管理し、JSON のリクエストとレスポンスをやりとりする。
//! プロセスが異常終了したら再起動して replay する。

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use leani_core::pure::{
    error_text, first_response, has_error, has_import, head_line, import_lines, indent, info_text,
    int, signal_name, sorries, strip_imports, toolchain_warning,
};
use leani_core::types::{EngineFault, Json};
use leani_read::config::{EnvConfig, read_text, which};
use leani_read::places::{BOOT_PROBE, Ctx, PROBE_IMPORT};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::{Pid, getpgid};

use crate::boot::{ensure_engine, lake_env, toolchain};

/// 応答を待つときに、Ctrl-C とプロセスの終了を確かめる間隔。
const POLL: Duration = Duration::from_millis(250);
/// EOF を読んだあと、終了ステータスが取れるまで待つ上限。
const REAP: Duration = Duration::from_millis(500);
/// 報告に使う宣言の先頭行の幅と、エラーの行数。
const HEAD_WIDTH: usize = 60;
const ERROR_LINES: usize = 3;

/// 取り消した宣言 1 件。元に戻すのに必要な情報だけを持つ。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Undone {
    pub env: Option<i64>,
    pub src: Option<String>,
    /// 保存したときのプロセスの世代。プロセスを再起動すると env id は無効になる。
    pub generation: u64,
}

/// `load_file` の結果。`src` は import を足したあとの、実際に送ったソース。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    pub resp: Json,
    pub bad: bool,
    pub src: String,
    /// probe_env が失敗した理由か、init を再適用できなかったときの理由。
    pub note: Option<String>,
}

/// replay の結果。環境に戻せなかったものもすべて含めて返す。
///
/// 件数だけを返すと、失敗した宣言も読み込み直せなかったファイルも、何も表示されずに
/// 消える。環境から無くなったものは必ず報告する。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Replay {
    /// 成功した宣言。
    pub done: Vec<String>,
    /// 失敗した宣言。
    pub failed: Vec<String>,
    /// 途中で打ち切ったので実行していない宣言。
    pub skipped: Vec<String>,
    /// init や `:l` で起きたこと。
    pub notes: Vec<String>,
    /// 最後に成功した宣言に残っている sorry。
    pub sorries: Vec<Json>,
}

impl Replay {
    /// 環境から無くなったものがあるか。
    pub fn dropped(&self) -> bool {
        !(self.failed.is_empty() && self.skipped.is_empty() && self.notes.is_empty())
    }
}

/// テスト用の差し込み口が、やりとりのどの時点で呼ばれたか。
#[derive(Clone, Copy, Debug)]
pub enum Stage<'a> {
    /// リクエストを書く前。
    Before(&'a Json),
    /// リクエストを書いた直後、レスポンスを読む前。
    Written(&'a Json),
    /// レスポンスを読んだ直後。
    Read(&'a Json, &'a Json),
}

/// テスト用の差し込み口が返す指示。
#[derive(Clone, Debug)]
pub enum Hooked {
    /// そのまま続ける。
    Pass,
    /// 残りを飛ばして、この応答を返す。`Read` のときは応答を置き換える。
    Reply(Json),
    /// Ctrl-C が押されたことにする。プロセスを止めて `Interrupted` を返す。
    Interrupt,
    /// プロセスを止めて、そのまま続ける (応答を読もうとして `Died` になる)。
    Kill,
}

/// テスト用の差し込み口。リクエストと応答を書き換えたり、途中で止めたりする。
///
/// テストは `Engine` に関数を入れ、`Engine` は `exchange` が決まった時点でその関数を
/// 呼ぶ。`Repl` が作るすべての `Engine` に同じ関数を渡せるように `Clone` ではなく
/// `Box` にする。
pub type ExchangeHook = Box<dyn FnMut(Stage<'_>) -> Hooked + Send>;

/// repl のサブプロセス 1 つ。異常終了したら `restart` で再起動する。
///
/// 状態 (env、base、stack、log、unplayed、init_src など) は `Engine` のメソッドだけが
/// 書き換える。`Repl` は読むためのメソッドだけを使う。
pub struct Engine {
    ctx: Ctx,
    cfg: EnvConfig,
    tc: String,
    dir: String,
    /// toolchain が違うときの警告。表示は `Repl` が出す。
    warning: Option<String>,
    proc_env: BTreeMap<String, String>,
    proc: Option<Child>,
    /// 今の env id。
    env: Option<i64>,
    /// 起動直後、または `:l` 直後 (init を含む) の env id。
    base: Option<i64>,
    /// `:undo` 用。
    stack: Vec<i64>,
    /// 受理した宣言。再起動時に replay する。
    log: Vec<String>,
    /// 入力したが env に追加されていない宣言。
    unplayed: Vec<String>,
    /// プロセスの世代。proofState の持ち主の確認に使う。
    generation: u64,
    /// `:l` したファイル。
    loaded: Option<String>,
    /// その中身 (import 行は除く)。
    loaded_src: Option<String>,
    /// そのファイルが書いていた import。
    loaded_imports: Vec<String>,
    /// init で実行したソース。
    init_src: Option<String>,
    hook: Option<ExchangeHook>,
}

impl Engine {
    /// エンジンを用意して、repl のプロセスを起動する。環境はまだ作らない (`boot`)。
    ///
    /// エンジンを用意する進み具合は `out` に出す。
    pub fn new(ctx: &Ctx, cfg: EnvConfig, out: &mut dyn Write) -> Result<Engine, EngineFault> {
        let tc = toolchain(ctx, &cfg)?;
        let dir = ensure_engine(ctx, out, cfg.engine.as_deref(), &tc, false)?;
        let warning = toolchain_warning(
            cfg.engine.is_some(),
            read_text(&format!("{dir}/lean-toolchain"))
                .unwrap_or_default()
                .trim(),
            &tc,
            &dir,
        );
        let proc_env = proc_env(ctx, &cfg, &tc, &dir)?;
        let mut eng = Engine {
            ctx: ctx.clone(),
            cfg,
            tc,
            dir,
            warning,
            proc_env,
            proc: None,
            env: None,
            base: None,
            stack: Vec::new(),
            log: Vec::new(),
            unplayed: Vec::new(),
            generation: 0,
            loaded: None,
            loaded_src: None,
            loaded_imports: Vec::new(),
            init_src: None,
            hook: None,
        };
        eng.spawn()?;
        Ok(eng)
    }

    // -- 読むためのメソッド -------------------------------------------------

    pub fn cfg(&self) -> &EnvConfig {
        &self.cfg
    }

    pub fn toolchain(&self) -> &str {
        &self.tc
    }

    pub fn dir(&self) -> &str {
        &self.dir
    }

    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    pub fn env(&self) -> Option<i64> {
        self.env
    }

    pub fn base(&self) -> Option<i64> {
        self.base
    }

    pub fn stack(&self) -> &[i64] {
        &self.stack
    }

    pub fn log(&self) -> &[String] {
        &self.log
    }

    pub fn unplayed(&self) -> &[String] {
        &self.unplayed
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn loaded(&self) -> Option<&str> {
        self.loaded.as_deref()
    }

    pub fn init_src(&self) -> Option<&str> {
        self.init_src.as_deref()
    }

    /// プロセスの id。テストがプロセスだけを kill するときに使う。
    pub fn pid(&self) -> Option<u32> {
        self.proc.as_ref().map(Child::id)
    }

    /// プロセスが実行中か。
    pub fn alive(&mut self) -> bool {
        match self.proc.as_mut() {
            Some(p) => matches!(p.try_wait(), Ok(None)),
            None => false,
        }
    }

    // -- テスト用 ---------------------------------------------------------------

    /// やりとりの差し込み口を入れる。テストと、`:env` で `Engine` を作り直す `Repl`
    /// が使う。
    #[doc(hidden)]
    pub fn set_hook(&mut self, hook: Option<ExchangeHook>) {
        self.hook = hook;
    }

    /// 差し込み口を外して返す。`:env` が新しい `Engine` に引き継ぐ。
    ///
    /// 差し込み口は `Engine` の値が持つので、`Repl` が `Engine` を作り直すときに
    /// 持ち越す。テストでなければ常に None。
    #[doc(hidden)]
    pub fn take_hook(&mut self) -> Option<ExchangeHook> {
        self.hook.take()
    }

    /// 環境を捨てて、boot に失敗した状態にする。テストだけが使う。
    #[cfg(feature = "testing")]
    #[doc(hidden)]
    pub fn drop_env(&mut self) {
        self.env = None;
    }

    /// env を直接置き換える。エンジンに無い env id を持った状態を作るテストだけが使う。
    #[cfg(feature = "testing")]
    #[doc(hidden)]
    pub fn set_env(&mut self, env: Option<i64>) {
        self.env = env;
    }

    // -- プロセス ---------------------------------------------------------------

    /// repl を起動する。再起動するたびに世代を進める。前のプロセスの proofState と
    /// 区別するため。
    fn spawn(&mut self) -> Result<(), EngineFault> {
        self.generation += 1;
        let elan = which(&self.ctx, "elan")
            .ok_or_else(|| EngineFault::Io("elan が PATH に無い".to_string()))?;
        let mut cmd = Command::new(elan);
        cmd.args([
            "run",
            &self.tc,
            &format!("{}/.lake/build/bin/repl", self.dir),
        ])
        .envs(&self.proc_env)
        .env("PATH", &self.ctx.path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
        // Ctrl-C が repl に届かないように、別のセッションにする。
        // SAFETY: setsid は fork のあと exec の前に呼んでよい async-signal-safe な
        // システムコールで、メモリを確保しない。
        unsafe {
            cmd.pre_exec(|| nix::unistd::setsid().map(|_| ()).map_err(io::Error::from));
        }
        self.proc = Some(cmd.spawn().map_err(|e| EngineFault::Io(e.to_string()))?);
        Ok(())
    }

    /// プロセスグループごと SIGKILL で止めて、回収する。
    pub fn kill(&mut self) {
        let Some(p) = self.proc.as_mut() else {
            return;
        };
        if !matches!(p.try_wait(), Ok(None)) {
            return;
        }
        let pid = Pid::from_raw(p.id().cast_signed());
        let group = getpgid(Some(pid)).and_then(|g| killpg(g, Signal::SIGKILL));
        if group.is_err() {
            let _ = p.kill();
        }
        let _ = reap(p, REAP);
    }

    // -- プロトコル -------------------------------------------------------------

    /// プロセスがどう終了したかをエラーメッセージに含める。
    ///
    /// バージョンの合わない .olean や使えなくなったエンジンを読み込むと、repl は何も
    /// 出力せずにシグナルで終了する。終了の仕方を記録しないと呼び出し側は理由を示せず、
    /// 「import に失敗した」という推測だけが表示される。ユーザーは正しく書いた
    /// import を疑うところから調べることになる。
    ///
    /// EOF を読んだ直後は、まだ終了ステータスを取得できないことがあるので、少し待つ。
    fn died(&mut self) -> EngineFault {
        let status = self.proc.as_mut().and_then(|p| reap(p, REAP));
        let how = match status {
            None => return EngineFault::Died("エンジンが応答しない".to_string()),
            Some(s) => match (s.signal(), s.code()) {
                (Some(sig), _) => format!("異常終了した ({})", signal_name(sig)),
                (None, Some(code)) => format!("終了した (exit {code})"),
                (None, None) => "終了した".to_string(),
            },
        };
        EngineFault::Died(format!(
            "エンジンが{how}\n  {}/.lake/build/bin/repl",
            self.dir
        ))
    }

    /// リクエストを 1 つ送って、レスポンスを 1 つ読む。
    ///
    /// 読み取りは fd を直接 poll で待って行う。バッファ付きの読み取りでは、Ctrl-C が
    /// どの時点で発生したのか (リクエストを送ったあとなのか、レスポンスを読み損ねた
    /// のか) が分からず、リクエストとレスポンスの対応がずれる恐れがある。中断した
    /// ときは、読んでいないレスポンスが残るので、このプロセスを止めてから
    /// `Interrupted` を返す。呼び出し側が再起動せずに次の `send` を呼んでも、古い
    /// レスポンスは読まず、`Died` になる。
    ///
    /// リクエストを書く前の中断 (差し込み口の `Before`) では、読んでいない
    /// レスポンスが無いので、プロセスは止めない。Ctrl-C のフラグは `read` でしか
    /// 確かめないので、実際の中断はすべて書いたあとに起きる。
    pub fn send(&mut self, obj: &Json) -> Result<Json, EngineFault> {
        if !self.alive() {
            return Err(self.died());
        }
        let got = self.hook(Stage::Before(obj));
        if let Some(done) = self.obey(got) {
            return done;
        }
        match self.exchange(obj) {
            Err(EngineFault::Interrupted) => {
                self.kill();
                Err(EngineFault::Interrupted)
            }
            other => other,
        }
    }

    /// 差し込み口を呼ぶ。入れていなければ `Pass`。
    fn hook(&mut self, stage: Stage<'_>) -> Hooked {
        match self.hook.as_mut() {
            Some(f) => f(stage),
            None => Hooked::Pass,
        }
    }

    /// 差し込み口の指示のうち、続ける以外のものを実行する。`None` なら続ける。
    fn obey(&mut self, got: Hooked) -> Option<Result<Json, EngineFault>> {
        match got {
            Hooked::Pass => None,
            Hooked::Reply(resp) => Some(Ok(resp)),
            Hooked::Interrupt => Some(Err(EngineFault::Interrupted)),
            Hooked::Kill => {
                self.kill();
                None
            }
        }
    }

    /// リクエストを書いてレスポンスを読む。差し込み口の `Before` は `send` が呼ぶ。
    fn exchange(&mut self, obj: &Json) -> Result<Json, EngineFault> {
        if let Err(e) = self.write(obj) {
            return Err(self.died_or(e));
        }
        let got = self.hook(Stage::Written(obj));
        if let Some(done) = self.obey(got) {
            return done;
        }
        let resp = self.read()?;
        let got = self.hook(Stage::Read(obj, &resp));
        match got {
            Hooked::Reply(other) => Ok(other),
            other => self.obey(other).unwrap_or(Ok(resp)),
        }
    }

    /// 書き込みの失敗は、プロセスの終了として報告する。
    fn died_or(&mut self, e: EngineFault) -> EngineFault {
        match e {
            EngineFault::Io(_) => self.died(),
            other => other,
        }
    }

    fn write(&mut self, obj: &Json) -> Result<(), EngineFault> {
        let stdin: &mut ChildStdin = self
            .proc
            .as_mut()
            .and_then(|p| p.stdin.as_mut())
            .ok_or_else(|| EngineFault::Io("stdin が無い".to_string()))?;
        let io = |e: io::Error| EngineFault::Io(e.to_string());
        stdin
            .write_all(format!("{obj}\n\n").as_bytes())
            .map_err(io)?;
        stdin.flush().map_err(io)
    }

    /// レスポンスを 1 つ読む。ためたバイト列全体を UTF-8 として読み直す。64 KB ごとに
    /// 変換すると、読み取りの境目にかかった `→` などが正しく読めなくなる。
    fn read(&mut self) -> Result<Json, EngineFault> {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 1 << 16];
        loop {
            if self
                .ctx
                .interrupted
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(EngineFault::Interrupted);
            }
            let ready = match self.wait_stdout() {
                Ok(ready) => ready,
                Err(_) => return Err(self.died()),
            };
            if !ready {
                if !self.alive() {
                    return Err(self.died());
                }
                continue;
            }
            let n = match self.stdout().and_then(|out| out.read(&mut chunk).ok()) {
                Some(n) if n > 0 => n,
                _ => return Err(self.died()),
            };
            buf.extend_from_slice(&chunk[..n]);
            if let Some(done) = first_response(&String::from_utf8_lossy(&buf)) {
                return Ok(done);
            }
        }
    }

    fn stdout(&mut self) -> Option<&mut ChildStdout> {
        self.proc.as_mut().and_then(|p| p.stdout.as_mut())
    }

    /// stdout が読めるようになるまで `POLL` だけ待つ。EINTR は「まだ」と同じ扱いにする
    /// (Ctrl-C で poll が中断されても、エンジンが死んだとは見なさない)。
    fn wait_stdout(&mut self) -> Result<bool, EngineFault> {
        let out = self
            .stdout()
            .ok_or_else(|| EngineFault::Io("stdout が無い".to_string()))?;
        let mut fds = [PollFd::new(out.as_fd(), PollFlags::POLLIN)];
        let timeout = PollTimeout::try_from(POLL).unwrap_or(PollTimeout::MAX);
        match poll(&mut fds, timeout) {
            Ok(n) => Ok(n > 0),
            Err(nix::errno::Errno::EINTR) => Ok(false),
            Err(e) => Err(EngineFault::Io(e.to_string())),
        }
    }

    /// コマンドを 1 つ送る。`fresh` は「新しい環境を作る」ことを明示する引数。
    ///
    /// env があるかどうかは、このメソッドだけで確認する (`probe_env` は env を明示して
    /// `send` を直接呼ぶ)。`env` キーを付けずに送ると、repl はエラーにせず、Init だけ
    /// の環境を新しく作ってその環境で答えてしまう。そのため「環境が無いときにどう
    /// するか」は必ず呼び出し側が決めることにして、決めていない呼び出しは送る前に
    /// エラーにする。boot が失敗した状態の扱いを書き忘れても、間違った結果を返すの
    /// ではなく `NoEnvironment` を返す。
    pub fn send_cmd(&mut self, src: &str, fresh: bool) -> Result<Json, EngineFault> {
        match (fresh, self.env) {
            (true, _) => self.send(&serde_json::json!({ "cmd": src })),
            (false, None) => Err(EngineFault::NoEnvironment(format!(
                "環境が無いのにコマンドを送ろうとした: {}",
                head_line(src, HEAD_WIDTH)
            ))),
            (false, Some(env)) => self.send(&serde_json::json!({ "cmd": src, "env": env })),
        }
    }

    pub fn send_tactic(&mut self, src: &str, state: i64) -> Result<Json, EngineFault> {
        self.send(&serde_json::json!({ "tactic": src, "proofState": state }))
    }

    /// info メッセージの中身だけを取得する。環境は進めない。
    ///
    /// 環境が無いときは「答えられなかった」として `None` を返す。送れば `send_cmd` が
    /// エラーにするが、型や補完の問い合わせは答えが無くても困らないので、エラーに
    /// せず `None` を返して扱いを呼び出し側に任せる。
    pub fn query(&mut self, src: &str) -> Result<Option<String>, EngineFault> {
        if self.env.is_none() {
            return Ok(None);
        }
        let resp = self.send_cmd(src, false)?;
        Ok(if has_error(&resp) {
            None
        } else {
            Some(info_text(&resp))
        })
    }

    // -- 宣言と環境 -------------------------------------------------------------

    /// 宣言が作った環境に進み、宣言を log に追加する。stack と log は 1 対 1。
    pub fn accept(&mut self, resp: &Json, src: &str) {
        if let Some(env) = self.env {
            self.stack.push(env);
        }
        self.env = int(resp, "env");
        self.log.push(src.to_string());
    }

    /// 直前の宣言を環境ごと取り消す。あとで元に戻せるよう、取り消した内容を返す。
    pub fn pop_decl(&mut self) -> Undone {
        let saved = Undone {
            env: self.env,
            src: self.log.pop(),
            generation: self.generation,
        };
        if let Some(env) = self.stack.pop() {
            self.env = Some(env);
        }
        saved
    }

    /// `pop_decl` で取り消したものを元に戻す。戻せなければ `false` を返す。
    ///
    /// 世代が変わっていたら戻さない。保存した env id は終了したプロセスのもので、
    /// 新しいエンジンには存在しない。それを今の env に設定すると、以後の cmd は
    /// 存在しない環境に送られ (repl は "Unknown environment." を返すだけ)、
    /// 何を入力しても何も起きなくなる。宣言は呼び出し側が再実行する。
    pub fn push_decl(&mut self, saved: Undone) -> bool {
        if saved.generation != self.generation {
            return false;
        }
        if let Some(src) = saved.src {
            self.log.push(src);
        }
        if let (Some(old), Some(now)) = (saved.env, self.env)
            && old != now
        {
            self.stack.push(now);
            self.env = Some(old);
        }
        true
    }

    /// 宣言をすべて捨てて base の環境に戻る。保留中の宣言も捨て、その件数を返す。
    ///
    /// base が無い (boot に失敗した) ときは `NoEnvironment` を返す。env に None を
    /// 設定し直しても、どの入力も送れない状態のまま保留だけが消える。
    pub fn reset(&mut self) -> Result<usize, EngineFault> {
        let Some(base) = self.base else {
            return Err(EngineFault::NoEnvironment(
                "元になる環境が無いのに :reset しようとした".to_string(),
            ));
        };
        let left = self.unplayed.len();
        self.env = Some(base);
        self.stack.clear();
        self.log.clear();
        self.unplayed.clear();
        Ok(left)
    }

    /// 環境の無いエンジンに、別のエンジンから引き継いだ宣言を保留として持たせる。
    ///
    /// `:env` で元の環境に戻るのを中断したときに使う。環境は無いので log には
    /// 入れられないが、保留にしておけば `:restart` が再実行し、`:save` もコメントとして
    /// 書き出す。
    pub fn carry(&mut self, decls: Vec<String>) {
        self.unplayed = decls;
    }

    /// `:save` が書き出すヘッダ。設定の import に、`:l` したファイルの import を追加する。
    ///
    /// 追加しないと、`:l` したファイルが import していたモジュールがヘッダに
    /// 含まれなくなる。その場合 `:save` は「宣言 n 件を書き出した」と成功を報告するのに、
    /// 書き出したファイルは `:l` でも lean でもエラーになる (Unknown identifier が並ぶ)。
    pub fn save_header(&self) -> String {
        let mut mods: Vec<&str> = vec!["Lean"];
        mods.extend(self.cfg.imports.iter().map(String::as_str));
        for m in &self.loaded_imports {
            if !mods.contains(&m.as_str()) {
                mods.push(m);
            }
        }
        mods.iter().map(|m| format!("import {m}\n")).collect()
    }

    /// 今の環境を構成しているソース。`:save` はこれを書き出す。
    ///
    /// log だけでは足りない。init と `:l` したファイルの内容は base の環境に含まれて
    /// いるので、それも並べないと、書き出したファイルを `:l` で読み込み直せない。
    ///
    /// 並べる順は実際に実行した順にする。`:l` は環境を作り直すので、init はその
    /// あとに実行される (`load_file` が再適用する)。逆の順に並べると、init が `:l`
    /// したファイルの名前を使っているときに、書き出したファイルがエラーになる。
    pub fn sources(&self) -> Vec<String> {
        [self.loaded_src.as_deref(), self.init_src.as_deref()]
            .into_iter()
            .flatten()
            .chain(self.log.iter().map(String::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    // -- 起動 / 再起動 ----------------------------------------------------------

    /// 設定された import を実行して、最初の環境を作る。
    ///
    /// pickle によるキャッシュは試したが速くならないので採用していない (DECISIONS.md)。
    pub fn boot(&mut self) -> Result<(), EngineFault> {
        self.env = None;
        self.stack.clear();
        let resp = self.send_cmd(&format!("{}{BOOT_PROBE}", self.cfg.boot_header()), true)?;
        if has_error(&resp) {
            // ヘッダにエラーがあるか、import が 1 つでも解決できずにヘッダ全体が
            // 無視されたか。後者の場合 repl はエラーを出さずに env を返すので、
            // BOOT_PROBE が失敗することでしか検出できない。そのまま起動すると
            // import Lean も無い環境になり、完結判定も補完も宣言もすべて失敗する。
            // repl のメッセージもそのまま表示する。toolchain を変更して .olean の
            // バージョンが一致しなくなったときは、このメッセージにしか情報が無い
            // (import 自体は正しく書かれている)。
            let why = error_text(&resp, ERROR_LINES);
            let mods = std::iter::once("Lean")
                .chain(self.cfg.imports.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            let head = format!("import に失敗した: {mods}");
            return Err(EngineFault::Died(if why.is_empty() {
                head
            } else {
                format!("{head}\n{}", indent(&why, "  "))
            }));
        }
        self.env = int(&resp, "env");
        self.base = self.env;
        Ok(())
    }

    /// 異常終了した、または中断されたエンジンを再起動し、宣言を replay する。
    pub fn restart(&mut self) -> Result<Replay, EngineFault> {
        self.kill();
        self.spawn()?;

        // boot が失敗しても、宣言は失わないようにする。やり直せば replay できる。
        // 前回実行できなかった宣言は env に無いので、成功した宣言の後ろに並べる。
        let mut log = self.log.clone();
        log.extend(self.unplayed.iter().cloned());
        let loaded = self.loaded.clone();
        if let Err(e) = self.boot() {
            self.abandon(log);
            return Err(e);
        }
        self.log.clear();
        self.unplayed.clear();

        // init と :l したファイルの内容は base の環境に含まれていたので、再適用する。
        // その途中で異常終了したか中断されたときも、boot の失敗と同じに扱う。
        // 読み込み直せなかった理由として報告すると、止まったエンジンに宣言を送る
        // ことになる。
        let mut notes = match self.reapply(loaded.as_deref()) {
            Ok(notes) => notes,
            Err(e) => {
                self.abandon(log);
                return Err(e);
            }
        };

        let mut out = self.replay(&log);
        notes.append(&mut out.notes);
        out.notes = notes;
        Ok(out)
    }

    /// 再起動が途中で失敗したときに、宣言を保留にする。
    ///
    /// プロセスは再起動したので前の env id は無効になっていて、宣言はどの環境にも
    /// 含まれていない。log に残すと stack の長さと一致しなくなり、:save が環境に
    /// 無い宣言を本体に書き出す。保留にすればコメントとして書き出され、ユーザーは
    /// 直してから :restart で再実行できる。base も捨てる。:reset が無効な id を
    /// 設定し直すと、submit は env が None かどうかしか見ないので検出できず、何を
    /// 入力してもエラーになる。
    fn abandon(&mut self, log: Vec<String>) {
        self.env = None;
        self.base = None;
        self.stack.clear();
        self.log.clear();
        self.unplayed = log;
    }

    /// init と `:l` したファイルの内容を、再起動した環境にもう一度入れる。
    /// 読み込み直せなかった理由と、init を再実行できなかった理由を返す。
    /// ファイルを読み込み直せた場合は、`load_file` の中で init も再適用される。
    fn reapply(&mut self, loaded: Option<&str>) -> Result<Vec<String>, EngineFault> {
        let lost = self.reload(loaded)?;
        let reload_failed = lost.is_some();
        let mut notes: Vec<String> = lost.into_iter().collect();
        if reload_failed || loaded.is_none() {
            notes.extend(self.reapply_init()?);
        }
        Ok(notes)
    }

    /// `:l` したファイルを読み込み直す。読み込み直せなかったときは理由を返す
    /// (`None` なら成功)。エンジンが異常終了したときや中断されたときは、ファイルを
    /// 覚えたまま `Err` を返す。次の `:restart` でまた読み込み直す。
    ///
    /// 読み込み直せなかったのに loaded_src を残すと、`sources` が env に無い宣言を
    /// 返し続ける。そのまま `:save` すると、書き出したファイルを `:l` したときに
    /// 「すでに宣言されている」というエラーになる。loaded_imports も同じで、残すと
    /// `:save` のヘッダに、もう環境に無いファイルの import を書く。
    fn reload(&mut self, loaded: Option<&str>) -> Result<Option<String>, EngineFault> {
        let Some(path) = loaded.filter(|p| !p.is_empty()) else {
            return Ok(None);
        };
        let why = match self.load_file(path, None) {
            Ok(out) if !out.bad => return Ok(out.note),
            Ok(_) => "読み込み直したらエラーになった".to_string(),
            Err(EngineFault::Io(why)) => why,
            Err(e) => return Err(e),
        };
        self.loaded = None;
        self.loaded_src = None;
        self.loaded_imports.clear();
        Ok(Some(format!("{path} を読み込み直せなかった: {why}")))
    }

    /// init のソースを今の環境で実行する。エラーが無ければ、その環境を base にし、
    /// 再起動したときに再実行できるよう src を init_src に保存する。
    ///
    /// エラーのときは状態を変えずに応答を返す。表示と init_src の扱いは呼び出し側が
    /// 決める。
    pub fn run_init(&mut self, src: &str) -> Result<Json, EngineFault> {
        let resp = self.send_cmd(src, false)?;
        if has_error(&resp) {
            return Ok(resp);
        }
        self.env = int(&resp, "env");
        self.base = self.env;
        self.init_src = Some(src.to_string());
        Ok(resp)
    }

    /// init を今の base の上でもう一度実行する。エラーになったら init_src を捨てて
    /// 理由を返す。エンジンが異常終了したときや中断されたときは、init_src を残した
    /// まま `Err` を返す。
    ///
    /// init_src を捨てずに残すと、`reload` と同じく `sources` が env に無い宣言を
    /// 返すようになる。
    fn reapply_init(&mut self) -> Result<Option<String>, EngineFault> {
        let Some(src) = self.init_src.clone() else {
            return Ok(None);
        };
        let resp = self.run_init(&src)?;
        if has_error(&resp) {
            self.init_src = None;
            return Ok(Some(
                "init ファイルを再実行したらエラーになった".to_string(),
            ));
        }
        Ok(None)
    }

    /// その環境で import が有効になっているかを確認する。有効でなければ理由を返す。
    ///
    /// import が 1 つでも解決できないと、repl はヘッダ全体を無視して (エラーも
    /// 出さずに) 環境を返す。boot と同じく `:l` でも BOOT_PROBE で検査しないと、
    /// 「読み込んだ」と報告したあと、完結判定も補完も宣言もすべて失敗する環境で
    /// 作業を続けることになる。`:save` も同じ import を書き出す。
    fn probe_env(&mut self, env: i64) -> Result<Option<String>, EngineFault> {
        let resp = self.send(&serde_json::json!({ "cmd": BOOT_PROBE, "env": env }))?;
        if !has_error(&resp) {
            return Ok(None);
        }
        let why = error_text(&resp, ERROR_LINES);
        let head = "解決できない import があったので、repl はファイルの import をすべて無視した";
        Ok(Some(if why.is_empty() {
            head.to_string()
        } else {
            format!("{head}: {}", head_line(&why, HEAD_WIDTH))
        }))
    }

    /// 宣言を今の環境でもう一度実行する。失敗したものは飛ばして続ける。
    ///
    /// `:restart` と、`:env` で環境を切り替えたあとに宣言を再実行する処理の両方が
    /// これを使う。失敗した宣言を何も表示せずに消さないよう、結果は `Replay` で返す。
    pub fn replay(&mut self, log: &[String]) -> Replay {
        if self.env.is_none() {
            // 実行する環境が無い。送れば send_cmd がエラーにするが、これは
            // 「どの宣言を環境に戻せたか」を返す関数なので、エラーを返さずに
            // 全件を保留にし、理由を Replay に含めて返す。
            let mut unplayed = log.to_vec();
            unplayed.append(&mut self.unplayed);
            self.unplayed = unplayed;
            return Replay {
                skipped: log.to_vec(),
                notes: vec!["環境が無いので再実行できない".to_string()],
                ..Replay::default()
            };
        }

        let mut out = Replay::default();
        for (n, src) in log.iter().enumerate() {
            let resp = match self.send_cmd(src, false) {
                Ok(resp) => resp,
                Err(_) => {
                    // まだ実行していない宣言は unplayed に保留する。エンジンを直せば
                    // :restart でやり直せる。ここで捨てると、入力した宣言が失われる。
                    //
                    // log に加えてはいけない。log は env に含まれている宣言の並びで、
                    // stack と 1 対 1 に対応している。env に無い宣言を加えると、
                    // :undo と sorry の置き換えが別の宣言を消してしまう。
                    out.skipped = log[n..].to_vec();
                    self.hold(&out.failed, &out.skipped);
                    return out;
                }
            };
            if has_error(&resp) {
                // 宣言のテキストは保留にする。失敗の原因はユーザーの操作ではない
                // ので、「再実行に失敗した」と報告するだけで、入力された宣言を消しては
                // いけない。読み込むファイルを直せば、次の :restart で成功する。
                out.failed.push(src.clone());
                continue;
            }
            self.accept(&resp, src);
            out.done.push(src.clone());
            // 最後に成功した宣言の sorry だけを保存する。証明中だった宣言は
            // log の末尾にあるので、これで :prove を再開できる。
            out.sorries = sorries(&resp).to_vec();
        }
        self.hold(&out.failed, &[]);
        out
    }

    /// 環境に戻せなかった宣言を、保留の先頭に並べる。
    fn hold(&mut self, failed: &[String], skipped: &[String]) {
        let mut unplayed = failed.to_vec();
        unplayed.extend_from_slice(skipped);
        unplayed.append(&mut self.unplayed);
        self.unplayed = unplayed;
    }

    /// ファイルを読み込んで、新しい環境を作る。`src` を渡せばファイルは読まない。
    ///
    /// 中断されたときとエンジンが異常終了したときは、`:l` を取り消して `Err` を返す。
    /// ファイルを読み込んだあとの init の再適用で止まったときも同じで、環境、宣言、
    /// 読み込んだファイルを `:l` の前に戻す。再起動すれば replay できる。
    pub fn load_file(&mut self, path: &str, src: Option<&str>) -> Result<Loaded, EngineFault> {
        let raw = match src {
            Some(s) => s.to_string(),
            None => std::fs::read_to_string(path).map_err(|e| EngineFault::Io(strerror(&e)))?,
        };
        let body = if has_import(&raw) {
            raw.clone()
        } else {
            format!("{}{raw}", self.cfg.header())
        };
        let src = format!("{PROBE_IMPORT}{body}");

        let keep = self.keep();
        self.env = None;

        let probed = self.send_cmd(&src, true).and_then(|resp| {
            let why = match (has_error(&resp), int(&resp, "env")) {
                (false, Some(env)) => self.probe_env(env)?,
                _ => None,
            };
            Ok((resp, why))
        });
        let (resp, why) = match probed {
            Ok(got) => got,
            Err(e) => {
                self.restore(keep);
                return Err(e);
            }
        };

        let bad = has_error(&resp) || why.is_some();
        let note = if bad {
            // 読み込みに失敗しても、今の環境は失わないようにする。env が None の
            // ままだと、以後の入力が import の無い環境に送られ、何を書いても
            // エラーになる。
            self.restore(keep);
            why
        } else {
            self.env = int(&resp, "env");
            self.base = self.env;
            self.loaded = Some(path.to_string());
            self.loaded_src = Some(strip_imports(&raw));
            self.loaded_imports = import_lines(&raw);
            // fresh で環境を作り直したので、init の内容は環境から消えている。
            // 再適用しないと、sources() が env に無い宣言を返し続ける。
            match self.reapply_init() {
                Ok(note) => note,
                Err(e) => {
                    self.restore(keep);
                    return Err(e);
                }
            }
        };
        Ok(Loaded {
            resp,
            bad,
            src,
            note,
        })
    }

    /// `:l` の前の状態を退避する。stack、log、unplayed は空にする。
    fn keep(&mut self) -> Kept {
        Kept {
            env: self.env,
            base: self.base,
            stack: std::mem::take(&mut self.stack),
            log: std::mem::take(&mut self.log),
            unplayed: std::mem::take(&mut self.unplayed),
            loaded: self.loaded.clone(),
            loaded_src: self.loaded_src.clone(),
            loaded_imports: self.loaded_imports.clone(),
        }
    }

    fn restore(&mut self, keep: Kept) {
        self.env = keep.env;
        self.base = keep.base;
        self.stack = keep.stack;
        self.log = keep.log;
        self.unplayed = keep.unplayed;
        self.loaded = keep.loaded;
        self.loaded_src = keep.loaded_src;
        self.loaded_imports = keep.loaded_imports;
    }
}

/// `:l` が退避する状態。読み込みに失敗したときと中断されたときに戻す。
struct Kept {
    env: Option<i64>,
    base: Option<i64>,
    stack: Vec<i64>,
    log: Vec<String>,
    unplayed: Vec<String>,
    loaded: Option<String>,
    loaded_src: Option<String>,
    loaded_imports: Vec<String>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.kill();
    }
}

/// repl に渡す環境変数。エンジンの .olean の場所を LEAN_PATH の先頭に足す。
///
/// `Command` は親の環境変数を引き継ぐので、ここでは lake env と LEAN_PATH だけを
/// 返す。元の LEAN_PATH は lake env のものを使い、lake env に無ければ起動時の
/// 環境変数 (`ctx.lean_path`) を使う。
pub fn proc_env(
    ctx: &Ctx,
    cfg: &EnvConfig,
    tc: &str,
    dir: &str,
) -> Result<BTreeMap<String, String>, EngineFault> {
    let mut env = lake_env(ctx, cfg.project.as_deref(), tc)?;
    let inherited = ctx.lean_path.clone().unwrap_or_default();
    let lean_path = env.get("LEAN_PATH").cloned().unwrap_or(inherited);
    env.insert(
        "LEAN_PATH".to_string(),
        format!("{dir}/.lake/build/lib/lean:{lean_path}"),
    );
    Ok(env)
}

/// 終了ステータスが取れるまで `limit` だけ待つ。
fn reap(p: &mut Child, limit: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + limit;
    loop {
        match p.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => return None,
        }
    }
}

/// OS のエラーを、エラー番号を付けずに理由だけにする。
pub(crate) fn strerror(e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(code) => nix::errno::Errno::from_raw(code).desc().to_string(),
        None => e.to_string(),
    }
}
