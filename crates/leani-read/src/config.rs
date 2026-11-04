//! 設定 (読み取り)。
//!
//! どの Lake プロジェクトの上で何を import して起動するか。ファイルと環境変数を
//! 見るが、何も書き換えない。

use std::fmt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::LazyLock;

use leani_core::pure::{abspath, same_toolchain};
use leani_core::types::ConfigError;
use regex::Regex;
use toml::{Table, Value};

use crate::places::{Ctx, ENGINE_REPO, PROBE_IMPORT, PROMPT};

/// どの Lake プロジェクトの上で何を import して起動するか。
///
/// ghci の ~/.ghci、ipython の profile と同じ位置づけ。特定のプロジェクト名を
/// コードに書かないため、名前は設定ファイル (config.toml) と CLI 引数だけに書く。
/// 設定が無くても、leani はカレントディレクトリの Lake プロジェクトから推測して
/// 起動する。
///
/// 作るときは値を整える `make` を使う。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvConfig {
    pub name: String,
    pub project: Option<String>,
    pub imports: Vec<String>,
    pub prompt: String,
    /// None なら leani が用意する。
    pub engine: Option<String>,
}

impl EnvConfig {
    /// 値を整えて作る。パスは絶対パスにし、空のプロンプトは既定のプロンプトにする。
    /// engine が無ければ LEANI_ENGINE を使う。
    pub fn make(
        ctx: &Ctx,
        name: &str,
        project: Option<&str>,
        imports: &[String],
        prompt: Option<&str>,
        engine: Option<&str>,
    ) -> EnvConfig {
        let abs = |p: Option<&str>| p.and_then(|p| abspath(p, &ctx.home, &ctx.cwd));
        let engine = engine.filter(|e| !e.is_empty()).or(ctx.engine.as_deref());
        EnvConfig {
            name: name.to_string(),
            project: abs(project),
            imports: imports.to_vec(),
            prompt: prompt
                .filter(|p| !p.is_empty())
                .unwrap_or(PROMPT)
                .to_string(),
            engine: abs(engine),
        }
    }

    /// 設定された import 行。起動時と、import の無いファイルの :l で使う。
    pub fn header(&self) -> String {
        self.imports
            .iter()
            .map(|m| format!("import {m}\n"))
            .collect()
    }

    pub fn boot_header(&self) -> String {
        format!("{PROBE_IMPORT}{}", self.header())
    }
}

impl fmt::Display for EnvConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mods = std::iter::once("Lean")
            .chain(self.imports.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        write!(f, "{} (import {mods})", self.name)
    }
}

// ------------------------------------------------------------------ 値の型

/// エラーで表示する型の名前。Python 版と同じ名前にする。
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "str",
        Value::Integer(_) => "int",
        Value::Float(_) => "float",
        Value::Boolean(_) => "bool",
        Value::Datetime(_) => "datetime",
        Value::Array(_) => "list",
        Value::Table(_) => "dict",
    }
}

/// 設定の文字列 1 つ。型が違えばエラーにする。
pub fn as_str(
    value: Option<&Value>,
    place: &str,
    config: &str,
) -> Result<Option<String>, ConfigError> {
    match value {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(v) => Err(ConfigError(format!(
            "{place} は文字列で書く (今は {}): {config}",
            type_name(v)
        ))),
    }
}

/// import のリスト。
///
/// 文字列 1 つをエラーにせずに受け取ると 1 文字ずつの import になる。どれも解決
/// できないので、repl はヘッダ全体を捨てる (import Lean も消える)。なのでエラーにする。
pub fn as_imports(
    value: Option<&Value>,
    place: &str,
    config: &str,
) -> Result<Vec<String>, ConfigError> {
    let strings = |xs: &[Value]| -> Option<Vec<String>> {
        xs.iter().map(|x| x.as_str().map(String::from)).collect()
    };
    match value {
        None => Ok(Vec::new()),
        Some(Value::String(s)) => Err(ConfigError(format!(
            "{place} は配列で書く: [\"{s}\"] ({config})"
        ))),
        Some(Value::Array(xs)) if strings(xs).is_some() => Ok(strings(xs).unwrap_or_default()),
        Some(_) => Err(ConfigError(format!(
            "{place} は文字列の配列で書く: {config}"
        ))),
    }
}

/// 設定の表 1 つ。型が違えばエラーにする。
pub fn as_table(value: Option<&Value>, place: &str, config: &str) -> Result<Table, ConfigError> {
    match value {
        None => Ok(Table::new()),
        Some(Value::Table(t)) => Ok(t.clone()),
        Some(v) => Err(ConfigError(format!(
            "{place} は表で書く (今は {}): {config}",
            type_name(v)
        ))),
    }
}

/// 設定に書いてある環境の名前を、名前の順に返す (`:env` の切り替え先の一覧)。
pub fn env_names(conf: &Table, config: &str) -> Result<Vec<String>, ConfigError> {
    as_table(conf.get("env"), "env", config).map(|t| t.keys().cloned().collect())
}

/// 設定ファイルに書いた環境の名前 (`:env` の補完の候補)。設定ファイルを読めなければ
/// 候補を出さない。
pub fn config_envs(ctx: &Ctx) -> Vec<String> {
    load_config(&ctx.config)
        .and_then(|conf| env_names(&conf, &ctx.config))
        .unwrap_or_default()
}

// ------------------------------------------------------------------ ファイル

/// 設定ファイルを読む。無ければ空の設定。
pub fn load_config(path: &str) -> Result<Table, ConfigError> {
    if !Path::new(path).is_file() {
        Ok(Table::new())
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|text| text.parse::<Table>().map_err(|e| e.to_string()))
            .map_err(|e| ConfigError(format!("設定ファイルを読めない: {path}\n  {e}")))
    }
}

/// lakefile を持つ一番近い親ディレクトリ。無ければ None。
pub fn lake_root(start: &str) -> Option<String> {
    Path::new(start)
        .ancestors()
        .find(|d| {
            ["lakefile.toml", "lakefile.lean"]
                .iter()
                .any(|f| d.join(f).is_file())
        })
        .map(|d| d.to_string_lossy().into_owned())
}

/// lakefile が公開しているライブラリ名。設定が無いときの import 候補。
pub fn lake_libs(project: &str) -> Vec<String> {
    let toml = format!("{project}/lakefile.toml");
    if Path::new(&toml).is_file() {
        toml_libs(&toml)
    } else {
        lean_libs(&format!("{project}/lakefile.lean"))
    }
}

/// lakefile.toml の lean_lib の名前。読めなければ空。
pub fn toml_libs(path: &str) -> Vec<String> {
    let data = read_text(path).and_then(|text| text.parse::<Table>().ok());
    let libs = data
        .as_ref()
        .and_then(|d| d.get("lean_lib"))
        .and_then(Value::as_array);
    libs.into_iter()
        .flatten()
        .filter_map(|lib| lib.get("name").and_then(Value::as_str).map(String::from))
        .collect()
}

static LEAN_LIB: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*lean_lib\s+«?([A-Za-z0-9_.']+)»?").expect("LEAN_LIB"));

/// lakefile.lean の lean_lib の名前。読めなければ空。
pub fn lean_libs(path: &str) -> Vec<String> {
    let text = read_text(path).unwrap_or_default();
    LEAN_LIB
        .captures_iter(&text)
        .filter_map(|m| m.get(1).map(|g| g.as_str().to_string()))
        .collect()
}

/// 読めなければ None。ファイルが無い場合と権限が無い場合を区別しない箇所で使う。
pub fn read_text(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// そのディレクトリの lean-toolchain。無ければ None。
pub fn read_toolchain(path: Option<&str>) -> Option<String> {
    let text = path.and_then(|p| read_text(&format!("{p}/lean-toolchain")))?;
    Some(text.trim().to_string()).filter(|tc| !tc.is_empty())
}

/// ctx の PATH から実行できるファイルを探す (Python の `shutil.which`)。
pub fn which(ctx: &Ctx, tool: &str) -> Option<String> {
    ctx.path
        .split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| format!("{dir}/{tool}"))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

// ------------------------------------------------------------------ 起動できるか

/// この環境で起動できない理由。無ければ None。
///
/// 起動時と :env の切り替え時の両方で使う。切り替えでは今のエンジンを
/// 終了させる前に呼ぶので、問題があれば今のセッションを残したままエラーにできる。
/// この関数は状態を見るだけで、何も変えない。
pub fn problem(ctx: &Ctx, cfg: &EnvConfig) -> Option<String> {
    project_problem(cfg)
        .or_else(|| tool_problem(ctx, cfg))
        .or_else(|| cfg.engine.as_deref().and_then(engine_problem))
        .or_else(|| version_problem(cfg))
}

fn project_problem(cfg: &EnvConfig) -> Option<String> {
    cfg.project
        .as_deref()
        .filter(|p| !Path::new(p).is_dir())
        .map(|p| format!("Lake プロジェクトが無い: {p}"))
}

fn tool_problem(ctx: &Ctx, cfg: &EnvConfig) -> Option<String> {
    let missing = |tool: &str| which(ctx, tool).is_none();
    if missing("elan") {
        Some("elan が PATH に無い".to_string())
    } else if missing("lake") {
        Some("lake が PATH に無い".to_string())
    } else if cfg.engine.is_none() && missing("git") {
        // leani がエンジンを用意する場合に必要なもの。取得する前に確認する。
        Some(format!(
            "git が PATH に無い。エンジン ({ENGINE_REPO}) の取得に使う"
        ))
    } else {
        None
    }
}

/// 指定されたエンジンは leani が管理しないので、必要なファイルがあるかだけ確認する。
fn engine_problem(engine: &str) -> Option<String> {
    let dir = Path::new(engine);
    if !dir.is_dir() {
        Some(format!("指定されたエンジンが無い: {engine}"))
    } else if !dir.join("lean-toolchain").is_file() {
        // エンジンの Lean のバージョンを決めるのはこのファイルだけ。無いと elan の
        // デフォルトの toolchain が使われ、エラーが出ないまま正しく実行できなくなる。
        Some(format!(
            "指定されたエンジンに lean-toolchain が無い: {engine}"
        ))
    } else if !dir.join(".lake/build/bin/repl").is_file() {
        Some(format!("repl が未ビルド: cd {engine} && lake build repl"))
    } else {
        None
    }
}

/// Lean のバージョンが違うエンジンは `.olean` を読めず、repl は起動直後に異常終了する。
/// 起動しても異常終了するだけなので、警告ではなくエラーにする。
fn version_problem(cfg: &EnvConfig) -> Option<String> {
    let proj = read_toolchain(cfg.project.as_deref())?;
    let eng = read_toolchain(cfg.engine.as_deref())?;
    let engine = cfg.engine.as_deref().unwrap_or("");
    if !same_toolchain(&proj, &eng) {
        Some(format!(
            "エンジンのバージョンがプロジェクトと違う (プロジェクト={proj} / エンジン={eng})。\n  \
             {proj} でビルドし直す: cd {engine} && echo {proj} > lean-toolchain && lake build repl\n  \
             または engine の指定 (env.<名前>.engine、engine、LEANI_ENGINE) を外して leani に用意させる"
        ))
    } else {
        None
    }
}

// ------------------------------------------------------------------ 環境の決め方

/// 名前で選んだ環境。設定に書いてある通りに使う (カレントディレクトリは見ない)。
pub fn from_config(
    ctx: &Ctx,
    table: &Table,
    name: &str,
    project: Option<&str>,
    imports: &[String],
    engine: Option<&str>,
) -> Result<EnvConfig, ConfigError> {
    let config = ctx.config.as_str();
    let Some(entry) = table.get(name) else {
        let known = table
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        let known = if known.is_empty() {
            "(1 つも無い)"
        } else {
            &known
        };
        return Err(ConfigError(format!(
            "環境 {name} は設定に無い: {config}\n  ある環境: {known}"
        )));
    };
    let place = |key: &str| format!("env.{name}.{key}");
    let entry = as_table(Some(entry), &format!("env.{name}"), config)?;
    // 引数で値を指定したときは、設定の値を読まないので、型も確かめない。
    let project = match project.filter(|p| !p.is_empty()) {
        Some(p) => Some(p.to_string()),
        None => as_str(entry.get("project"), &place("project"), config)?,
    };
    let imports = match imports {
        [] => as_imports(entry.get("imports"), &place("imports"), config)?,
        _ => imports.to_vec(),
    };
    let prompt = as_str(entry.get("prompt"), &place("prompt"), config)?;
    let own_engine = as_str(entry.get("engine"), &place("engine"), config)?;
    Ok(EnvConfig::make(
        ctx,
        name,
        project.as_deref(),
        &imports,
        prompt.as_deref(),
        own_engine.as_deref().filter(|e| !e.is_empty()).or(engine),
    ))
}

/// 名前で環境を選ばないときに推測する。-e も default も無いときと、-e を付けずに
/// -p か -i を付けたときに使う。-p が無ければ、カレントディレクトリから lakefile を
/// 持つ親ディレクトリを探す。-i が無ければ、その lean_lib を import する。
/// Lake プロジェクトの外なら Lean 本体だけで起動する。
pub fn guess_env(
    ctx: &Ctx,
    project: Option<&str>,
    imports: &[String],
    engine: Option<&str>,
) -> EnvConfig {
    // -p "" はカレントディレクトリを探さず、Lake プロジェクトの外として扱う。
    let root = match project {
        Some(p) => Some(p.to_string()),
        None => lake_root(&ctx.cwd),
    }
    .filter(|r| !r.is_empty());
    let mods = match (imports, &root) {
        ([], Some(r)) => lake_libs(r),
        _ => imports.to_vec(),
    };
    let name = match &root {
        Some(r) => r.rsplit('/').next().unwrap_or(r),
        None => "plain",
    };
    EnvConfig::make(ctx, name, root.as_deref(), &mods, None, engine)
}

/// CLI 引数と設定ファイルから、起動する環境を 1 つ決める。
///
/// 優先順は 引数 > 名前付き環境 > default > カレントディレクトリの Lake プロジェクト。
pub fn resolve(
    ctx: &Ctx,
    name: Option<&str>,
    project: Option<&str>,
    imports: &[String],
    conf: &Table,
) -> Result<EnvConfig, ConfigError> {
    let config = ctx.config.as_str();
    let table = as_table(conf.get("env"), "env", config)?;
    let engine = as_str(conf.get("engine"), "engine", config)?;
    // 名前も -i も -p も無いときだけ default を使う。
    let chosen = match (name, imports, project.filter(|p| !p.is_empty())) {
        (None, [], None) => as_str(conf.get("default"), "default", config)?,
        _ => name.map(String::from),
    };
    match chosen {
        Some(chosen) => from_config(ctx, &table, &chosen, project, imports, engine.as_deref()),
        None => Ok(guess_env(ctx, project, imports, engine.as_deref())),
    }
}
