//! コマンドラインの引数 (読み取り)。
//!
//! 引数を読むだけで、終了はしない。エラーは文字列で返し、表示と終了は bin が行う。

/// CLI 引数を読んだ結果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// -e
    pub name: Option<String>,
    /// -p
    pub project: Option<String>,
    /// -i (繰り返せる)
    pub imports: Vec<String>,
    /// 起動時に読み込むファイル
    pub preload: Option<String>,
    /// --setup
    pub setup: bool,
    pub version: bool,
    pub help: bool,
}

/// 引数を読む。値の無いオプションはその場でエラーにする。
///
/// エラーのときは、表示するメッセージ (`leani: ` を除いた部分) を返す。
pub fn parse_args<S: AsRef<str>>(argv: &[S]) -> Result<Args, String> {
    let mut args = Args::default();
    let mut rest = argv.iter().map(AsRef::as_ref);
    while let Some(a) = rest.next() {
        let mut value = |what: &str| {
            rest.next()
                .map(String::from)
                .ok_or_else(|| format!("{a} には{what}が必要"))
        };
        match a {
            "-e" | "--env" => args.name = Some(value("名前")?),
            "-i" | "--import" => args.imports.push(value("モジュール名")?),
            "-p" | "--project" => args.project = Some(value("ディレクトリ")?),
            "--setup" => args.setup = true,
            "-h" | "--help" => args.help = true,
            "-V" | "--version" => args.version = true,
            _ if a.starts_with('-') => {
                return Err(format!("不明なオプション: {a}  (-h で使い方)"));
            }
            _ => args.preload = Some(a.to_string()),
        }
    }
    Ok(args)
}
