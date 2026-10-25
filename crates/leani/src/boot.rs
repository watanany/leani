//! 起動の用意 (副作用)。
//!
//! Lean のバージョンを決め、エンジン (leanprover-community/repl) を clone してビルドし、
//! lake の環境変数を用意する。外部コマンドは `Ctx` の PATH で探し、進み具合の表示は
//! 引数の `out` に書く。テストでは `out` を文字列にして受け取る。

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::LazyLock;
use std::sync::atomic::Ordering;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use leani_core::pure::{
    Style, engine_dir, parse_env_lines, pick_tag, toolchain_version, version_key,
};
use leani_core::types::EngineError;
use leani_read::config::{EnvConfig, read_text, read_toolchain, which};
use leani_read::places::{Ctx, ENGINE_REPO};
use regex::Regex;

static LEAN_VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"version (\d+\.\d+\.\d+(?:-rc\d+)?)").expect("regex"));

/// 進み具合を 1 行、薄い色で表示する。表示に失敗しても起動の用意は続ける。
fn say(out: &mut dyn Write, ctx: &Ctx, text: &str) {
    let style = Style { color: ctx.color };
    let _ = writeln!(out, "{}", style.dim(text));
    let _ = out.flush();
}

// ------------------------------------------------------------------ 外部コマンド

/// 外部コマンドが終わらなかった理由。
enum Stopped {
    /// `timeout` のあいだに終わらなかった。
    Timeout,
    /// Ctrl-C。同じプロセスグループなので、子プロセスにも届いている。
    Interrupted,
    /// 実行できなかった (PATH に無い、など)。
    Spawn(std::io::Error),
}

/// 終わった外部コマンド。出力をためなかったときは stdout と stderr は空。
struct Finished {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

/// `ctx` の PATH にある `tool` を実行し、終わるのを待つ。
///
/// `Command` はプロセスの PATH で実行ファイルを探すので、`ctx` の PATH で探すのは
/// ここで行う (テストでは偽の git や lake を置いた PATH を渡す)。`capture` なら出力を
/// ためて返し、そうでなければ画面にそのまま出す。`timeout` のあいだに終わらないか、
/// 待っているあいだに Ctrl-C が押されたら、子プロセスを kill して `Stopped` を返す。
/// git が認証を求めて止まらないように `GIT_TERMINAL_PROMPT=0` を渡す。
fn run(
    ctx: &Ctx,
    tool: &str,
    args: &[&str],
    cwd: Option<&str>,
    capture: bool,
    timeout: Option<Duration>,
) -> Result<Finished, Stopped> {
    let exe = which(ctx, tool).ok_or_else(|| {
        Stopped::Spawn(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{tool} が PATH に無い"),
        ))
    })?;
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .env("PATH", &ctx.path)
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    if capture {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    let mut child = cmd.spawn().map_err(Stopped::Spawn)?;
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let status = wait(ctx, &mut child, timeout)?;
    Ok(Finished {
        status,
        stdout: stdout.map(collect).unwrap_or_default(),
        stderr: stderr.map(collect).unwrap_or_default(),
    })
}

/// パイプを別スレッドで最後まで読む。子プロセスが出力でいっぱいになって止まらないように。
fn drain(mut pipe: impl Read + Send + 'static) -> JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
}

fn collect(reader: JoinHandle<String>) -> String {
    reader.join().unwrap_or_default()
}

/// 子プロセスが終わるのを、タイムアウトと Ctrl-C を確かめながら待つ。
fn wait(ctx: &Ctx, child: &mut Child, timeout: Option<Duration>) -> Result<ExitStatus, Stopped> {
    let deadline = timeout.map(|t| Instant::now() + t);
    loop {
        if let Some(status) = child.try_wait().map_err(Stopped::Spawn)? {
            return Ok(status);
        }
        let stopped = if ctx.interrupted.swap(false, Ordering::SeqCst) {
            Stopped::Interrupted
        } else if deadline.is_some_and(|d| Instant::now() >= d) {
            Stopped::Timeout
        } else {
            thread::sleep(Duration::from_millis(20));
            continue;
        };
        let _ = child.kill();
        let _ = child.wait();
        return Err(stopped);
    }
}

fn secs(d: Duration) -> u64 {
    d.as_secs()
}

// ------------------------------------------------------------------ バージョン

/// elan が今選択しているバージョン。プロジェクトも指定も無いときに使う。
pub fn local_toolchain(ctx: &Ctx) -> Option<String> {
    let got = run(
        ctx,
        "lean",
        &["--version"],
        None,
        true,
        Some(ctx.setup_timeout),
    )
    .ok()?;
    LEAN_VERSION
        .captures(&got.stdout)
        .map(|m| format!("leanprover/lean4:v{}", &m[1]))
}

/// 使う Lean のバージョン。プロジェクト、ユーザーが指定したエンジン、elan の
/// デフォルトの順に探す。
///
/// 2 番目はユーザーが自分で指定したエンジン用。leani はそれをビルドし直さないので、
/// 合わせるべきバージョンは「そのエンジンをビルドしたバージョン」しかない。leani が
/// 用意したエンジンなら engine が None なので、2 番目は飛ばして elan のデフォルトの
/// バージョンを使う。
pub fn guess_toolchain(ctx: &Ctx, cfg: &EnvConfig) -> Option<String> {
    read_toolchain(cfg.project.as_deref())
        .or_else(|| read_toolchain(cfg.engine.as_deref()))
        .or_else(|| local_toolchain(ctx))
}

/// 使うバージョンを決める。分からなければ leani は起動を中止する。
///
/// lean はカレントディレクトリの lean-toolchain を見てバージョンを決めるので、
/// プロジェクトの外から実行するとデフォルトのバージョンが選ばれて .olean を読めなく
/// なる。ここで決めたバージョンを elan run で固定し、エンジンもそのバージョンで
/// ビルドする。
pub fn toolchain(ctx: &Ctx, cfg: &EnvConfig) -> Result<String, EngineError> {
    guess_toolchain(ctx, cfg).ok_or_else(|| {
        EngineError(
            "Lean のバージョンが分からない (lean-toolchain が無く、lean --version からも取得できない)"
                .to_string(),
        )
    })
}

// ------------------------------------------------------------------ エンジン

/// 自動で用意できなかったときに表示する手順。
///
/// タグを省くと HEAD を clone することになりバージョンが合わないので、必ずタグを
/// 指定する。使うバージョンでビルドし直すところまで含めて、ensure_engine と同じ
/// ことを手動で行う手順にする。
pub fn manual_setup(path: &str, tc: &str, tag: Option<&str>) -> String {
    let branch = tag.map_or_else(
        || format!("<{} 以下で一番新しいタグ>", toolchain_version(tc)),
        str::to_string,
    );
    format!(
        "手動で用意する場合:\n  \
         git clone --branch {branch} {ENGINE_REPO} {path}\n  \
         echo {tc} > {path}/lean-toolchain\n  \
         cd {path} && lake build repl"
    )
}

/// repl のタグ一覧。clone せずにリモートに問い合わせる。
pub fn fetch_tags(ctx: &Ctx) -> Result<Vec<String>, EngineError> {
    let args = ["ls-remote", "--tags", "--refs", ENGINE_REPO];
    let got = run(ctx, "git", &args, None, true, Some(ctx.setup_timeout)).map_err(|e| match e {
        Stopped::Timeout => EngineError(format!(
            "repl のタグの取得が {} 秒で終わらなかった",
            secs(ctx.setup_timeout)
        )),
        Stopped::Spawn(e) => EngineError(format!("git を実行できなかった: {e}")),
        Stopped::Interrupted => EngineError("^C 中断した".to_string()),
    })?;
    if !got.status.success() {
        return Err(EngineError(format!(
            "repl のタグを取得できなかった:\n{}",
            got.stderr.trim()
        )));
    }
    // 各行は "<sha>\trefs/tags/v4.33.0" の形。タグ名だけを取り出す。
    Ok(got
        .stdout
        .split_whitespace()
        .filter_map(|r| r.strip_prefix("refs/tags/"))
        .map(|r| r.rsplit('/').next().unwrap_or(r).to_string())
        .collect())
}

/// エンジンの用意に使う外部コマンドを実行する。出力はそのまま表示する。
pub fn run_setup(
    ctx: &Ctx,
    out: &mut dyn Write,
    cmd: &[&str],
    cwd: Option<&str>,
) -> Result<(), EngineError> {
    let line = cmd.join(" ");
    say(out, ctx, &format!("  {line}"));
    // ビルドには時間がかかるので、タイムアウトは設けない。
    let got = run(ctx, cmd[0], &cmd[1..], cwd, false, None).map_err(|e| match e {
        Stopped::Spawn(e) => EngineError(format!("{} を実行できなかった: {e}", cmd[0])),
        Stopped::Interrupted => EngineError(format!("^C 中断した: {line}")),
        Stopped::Timeout => EngineError(format!("終わらなかった: {line}")),
    })?;
    match got.status.code() {
        _ if got.status.success() => Ok(()),
        Some(code) => Err(EngineError(format!("失敗した (exit {code}): {line}"))),
        None => Err(EngineError(format!("失敗した ({}): {line}", got.status))),
    }
}

fn has_repl(dir: &str) -> bool {
    Path::new(dir).join(".lake/build/bin/repl").is_file()
}

/// タグのソースを、使うバージョンでビルドして配置する。
///
/// .olean を読めるかどうかはビルドに使った Lean のバージョンで決まるので、clone
/// した lean-toolchain を書き換えてからビルドする。ビルドに成功してから rename で
/// 配置するので、途中で中断しても不完全なディレクトリが残らない。
pub fn build_engine(
    ctx: &Ctx,
    out: &mut dyn Write,
    path: &str,
    tc: &str,
    tag: &str,
) -> Result<(), EngineError> {
    let version = toolchain_version(tc);
    let note = if tag == version {
        String::new()
    } else {
        format!(" (タグ {tag} を {version} でビルドする)")
    };
    say(
        out,
        ctx,
        &format!("エンジンを用意する (初回のみ): {version}{note}"),
    );

    // leani を 2 つ同時に起動しても衝突しないよう、作業用のディレクトリは呼び出しごとに
    // 分ける。この関数を抜けるときに (配置したあとも、失敗したときも) 消す。
    let place = Path::new(path);
    let parent = place.parent().unwrap_or(place);
    let name = place.file_name().map(|n| n.to_string_lossy().into_owned());
    fs::create_dir_all(parent).map_err(|e| {
        EngineError(format!(
            "ディレクトリを作れなかった ({}): {e}",
            parent.display()
        ))
    })?;
    let work = tempfile::Builder::new()
        .prefix(&format!("{}.", name.unwrap_or_default()))
        .suffix(".tmp")
        .tempdir_in(parent)
        .map_err(|e| {
            EngineError(format!(
                "作業用のディレクトリを作れなかった ({}): {e}",
                parent.display()
            ))
        })?;
    let tmp = work.path().to_string_lossy().into_owned();

    run_setup(
        ctx,
        out,
        &[
            "git",
            "clone",
            "--depth",
            "1",
            "--branch",
            tag,
            ENGINE_REPO,
            &tmp,
        ],
        None,
    )?;
    fs::write(format!("{tmp}/lean-toolchain"), format!("{tc}\n"))
        .map_err(|e| EngineError(format!("lean-toolchain を書けなかった ({tmp}): {e}")))?;
    run_setup(ctx, out, &["lake", "build", "repl"], Some(&tmp))?;
    if !has_repl(&tmp) {
        return Err(EngineError(format!("ビルドしたのに repl が無い: {tmp}")));
    }

    // ビルドしているあいだに別の leani がエンジンを配置していたら、そのエンジンは
    // 使用中かもしれないので、消さずに使う。
    if has_repl(path) {
        say(
            out,
            ctx,
            &format!("別の leani がエンジンを用意していた: {path}"),
        );
        return Ok(());
    }
    let _ = fs::remove_dir_all(path);
    fs::rename(&tmp, path)
        .map_err(|e| EngineError(format!("エンジンを配置できなかった ({path}): {e}")))?;
    say(out, ctx, &format!("用意した: {path}"));
    Ok(())
}

/// ビルドに使うタグ。無ければ理由を返す。
fn choose_tag(ctx: &Ctx, out: &mut dyn Write, version: &str) -> Result<String, EngineError> {
    match (pick_tag(version, &fetch_tags(ctx)?), version_key(version)) {
        (Some(tag), _) => Ok(tag),
        (None, None) => {
            // nightly や stable。バージョンとして解釈できないので比較できない。repl の
            // master は最新の Lean に追従しているので、master で試す。
            say(
                out,
                ctx,
                &format!("{version} に対応するタグは無い。master で試す"),
            );
            Ok("master".to_string())
        }
        (None, Some(_)) => Err(EngineError(format!("repl に {version} 用のタグが無い"))),
    }
}

/// 自動で用意できなくても、手動なら成功することがある。使うタグも含めて手順を添える。
fn with_manual(e: EngineError, path: &str, tc: &str, tag: Option<&str>) -> EngineError {
    EngineError(format!("{}\n{}", e.0, manual_setup(path, tc, tag)))
}

/// 使うエンジンのディレクトリを返す。leani が管理するエンジンは、無ければ用意する。
///
/// ユーザーが engine を指定したときは leani の管理外なので、ビルド済みかを確認する
/// だけで、何も作らないし消さない。build_engine は配置先のディレクトリを作り直すので、
/// ユーザーが指定したディレクトリに対して呼んではいけない。`asked` は `--setup` で
/// 明示的に頼まれたときで、LEANI_NO_SETUP があっても用意する。
pub fn ensure_engine(
    ctx: &Ctx,
    out: &mut dyn Write,
    engine: Option<&str>,
    tc: &str,
    asked: bool,
) -> Result<String, EngineError> {
    let path = engine_dir(engine, tc, &ctx.engine_cache)?;
    if has_repl(&path) {
        return Ok(path);
    }
    if engine.is_some() {
        return Err(EngineError(format!(
            "repl が未ビルド: cd {path} && lake build repl"
        )));
    }
    if ctx.no_setup && !asked {
        return Err(EngineError(format!(
            "エンジンが無い: {path}\n  \
             LEANI_NO_SETUP が設定されているので自動で用意しない (leani --setup で用意する)\n{}",
            manual_setup(&path, tc, None)
        )));
    }
    let tag =
        choose_tag(ctx, out, toolchain_version(tc)).map_err(|e| with_manual(e, &path, tc, None))?;
    build_engine(ctx, out, &path, tc, &tag).map_err(|e| with_manual(e, &path, tc, Some(&tag)))?;
    Ok(path)
}

// ------------------------------------------------------------------ lake env

const LAKE_ENV_KEYS: [&str; 4] = [
    "LEAN_PATH",
    "LEAN_SRC_PATH",
    "DYLD_LIBRARY_PATH",
    "LD_LIBRARY_PATH",
];

/// lake env が設定する環境変数。
///
/// lake env は起動に 1 秒近くかかるので、新しいキャッシュがあれば再利用する。
/// LEAN_PATH は core の .olean を指すので、toolchain ごとに別のキーでキャッシュする。
/// 同じキーを使うと、rc のバージョンを変更したときに前のバージョンの .olean を
/// 指したままになり、repl は起動するのに import がすべて失敗する。
pub fn lake_env(
    ctx: &Ctx,
    project: Option<&str>,
    tc: &str,
) -> Result<BTreeMap<String, String>, EngineError> {
    let Some(project) = project.filter(|p| !p.is_empty()) else {
        return Ok(BTreeMap::new());
    };
    let key = sha1_smol::Sha1::from(format!("{project}\n{tc}"))
        .digest()
        .to_string();
    let name = Path::new(project)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let cache = format!("{}/lake-env/{name}-{}", ctx.state, &key[..12]);
    if lake_env_stale(&cache, project) {
        write_lake_env(ctx, &cache, project)?;
    }
    Ok(parse_env_lines(&read_text(&cache).unwrap_or_default()))
}

// これらのファイルがキャッシュより新しければ取得し直す。manifest は依存パッケージの
// バージョン、lean-toolchain は core のバージョンを決める。どちらが変わっても
// LEAN_PATH は変わる。
const LAKE_ENV_INPUTS: [&str; 2] = ["lake-manifest.json", "lean-toolchain"];

/// キャッシュが無いか、プロジェクトのファイルより古いか。判断できなければ取得し直す
/// (確認しているあいだにファイルが削除されることがある)。
pub fn lake_env_stale(cache: &str, project: &str) -> bool {
    let age = fs::metadata(cache)
        .ok()
        .filter(|m| m.is_file())
        .and_then(|m| m.modified().ok());
    let Some(age) = age else {
        return true;
    };
    LAKE_ENV_INPUTS
        .iter()
        .any(|name| match fs::metadata(format!("{project}/{name}")) {
            Ok(m) if m.is_file() => match m.modified() {
                Ok(at) => at > age,
                Err(_) => true,
            },
            _ => false,
        })
}

/// lake env を実行して、結果をキャッシュに保存する。
pub fn write_lake_env(ctx: &Ctx, cache: &str, project: &str) -> Result<(), EngineError> {
    let script = LAKE_ENV_KEYS
        .iter()
        .map(|k| format!("printf \"{k}=%s\\n\" \"${{{k}-}}\""))
        .collect::<Vec<_>>()
        .join("\n");
    let args = ["env", "sh", "-c", &script];
    let got = run(
        ctx,
        "lake",
        &args,
        Some(project),
        true,
        Some(ctx.setup_timeout),
    )
    .map_err(|e| match e {
        Stopped::Timeout => EngineError(format!(
            "lake env が {} 秒で終わらなかった ({project})",
            secs(ctx.setup_timeout)
        )),
        Stopped::Spawn(e) => EngineError(format!("lake env を実行できなかった ({project}): {e}")),
        Stopped::Interrupted => EngineError(format!("^C 中断した: lake env ({project})")),
    })?;
    if !got.status.success() {
        return Err(EngineError(format!(
            "lake env が失敗した ({project}):\n{}",
            got.stderr.trim()
        )));
    }
    save(cache, &got.stdout)
        .map_err(|e| EngineError(format!("lake env を保存できなかった ({cache}): {e}")))
}

/// 一時ファイルに書いてから rename で置く。同時に起動した別の leani と同じファイルに
/// 書き込まないため。失敗したら一時ファイルは消す。
fn save(cache: &str, text: &str) -> std::io::Result<()> {
    let place = Path::new(cache);
    let dir = place.parent().unwrap_or(place);
    fs::create_dir_all(dir)?;
    let mut file = tempfile::Builder::new().suffix(".tmp").tempfile_in(dir)?;
    file.write_all(text.as_bytes())?;
    file.persist(cache).map(|_| ()).map_err(|e| e.error)
}

// ------------------------------------------------------------------ まとめ

/// その環境で leani を起動できるようにする。エンジンが無ければここで用意する。
///
/// Engine を作る前に呼べる。:env の切り替えでは今のエンジンを終了させる前に
/// 実行するので、ここでエラーになった場合は何も変えずに済む。lake env の結果も
/// 先に取得してキャッシュしておく (Engine の中で失敗させない)。
pub fn prepare(ctx: &Ctx, out: &mut dyn Write, cfg: &EnvConfig) -> Result<(), EngineError> {
    let tc = toolchain(ctx, cfg)?;
    ensure_engine(ctx, out, cfg.engine.as_deref(), &tc, false)?;
    lake_env(ctx, cfg.project.as_deref(), &tc)?;
    Ok(())
}
