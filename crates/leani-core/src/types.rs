//! 送り方のラベル、入力の状態、証明モードの状態、エラー (純粋)。
//!
//! repl の応答は `serde_json::Value` のまま扱い、struct にはしない。キーが無い、
//! 型が違うといった応答でも、読む箇所で既定値に置き換えて先に進む (DECISIONS.md の
//! 「pydantic で応答を検査する」を採用しなかった理由と同じ)。

use thiserror::Error;

/// repl の応答 1 つ、またはパーサの応答。
pub type Json = serde_json::Value;

/// 入力をエンジンにどう送るか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// command としてそのまま送る。
    Cmd,
    /// 項なので `#eval` に包んで送る。
    Term,
    /// 証明モードのタクティク。
    Tac,
}

/// 入力の状態。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// 完結している。
    Complete,
    /// 続きの行を待つ。
    More,
    /// パースできない。そのまま送って Lean にエラーを出させる。
    Err,
}

/// 証明モードの状態。1 行 = 1 タクティクで進む。タクティクを進めたり取り消したり
/// するときは、新しい `Proof` を作る (`pure::tactic_step` と `pure::tactic_undo`)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proof {
    /// repl 側の proofState。
    pub state: i64,
    pub goals: Vec<String>,
    /// この proofState を作ったエンジンの世代。
    pub generation: u64,
    pub script: Vec<String>,
    /// `:undo` 用。タクティクを実行する前の proofState とゴール。ゴールも戻さないと、
    /// `:undo` のあとの `:goals` が取り消したタクティクのあとのゴールを表示する。
    pub stack: Vec<(i64, Vec<String>)>,
}

impl Proof {
    /// タクティクをまだ 1 つも実行していない状態。
    pub fn new(state: i64, goals: Vec<String>, generation: u64) -> Self {
        Proof {
            state,
            goals,
            generation,
            script: Vec::new(),
            stack: Vec::new(),
        }
    }
}

/// Repl が表示を出す先。端末と Jupyter で実装を差し替える。
pub trait Output: Send {
    /// 改行を付けずに表示を出す。
    fn put(&mut self, text: &str);

    /// 普通の表示を 1 行出す。
    fn write(&mut self, text: &str) {
        self.put(text);
        self.put("\n");
    }

    /// エラーを出す。Jupyter ではセルの結果をエラーにする。
    fn fail(&mut self, text: &str);

    /// `:!` で shell のコマンドを実行し、その出力を表示する。
    fn shell(&mut self, cmd: &str);

    /// 長くなることがある表示 (`:abbrev` の一覧)。端末では pager で表示する。
    fn page(&mut self, text: &str);
}

/// エンジンを用意できない。起動時なら起動を中止する。起動後なら REPL は続ける。
#[derive(Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct EngineError(pub String);

/// 設定が読めない、または指定された環境が無い。
#[derive(Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct ConfigError(pub String);

/// エンジンとのやりとりが失敗した理由。
///
/// Python 版では例外ごとに型が分かれていた (`EngineDied`、`Interrupted`、
/// `NoEnvironment`、`OSError`)。Rust では 1 つの enum にして、呼び出し側が `match` で
/// 分ける。raise する箇所と except する箇所が別のモジュールなので、どちらでもない
/// ここに置く。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum EngineFault {
    /// エンジンを用意できない。起動時なら起動を中止する。起動後なら REPL は続ける。
    #[error("{0}")]
    Setup(String),
    /// repl のプロセスが応答しなくなった。呼び出し側は repl を再起動する。
    #[error("{0}")]
    Died(String),
    /// 評価中に Ctrl-C が押された。repl とのやりとりがずれているので、呼び出し側は
    /// repl を再起動する。
    #[error("^C")]
    Interrupted,
    /// 環境が無いのに、環境の上で入力を実行しようとした。呼び出し側のバグ。
    ///
    /// 環境を指定せずに送ると、repl は Init だけの環境を作って答える (型が正しくない、
    /// 宣言が消える)。エラーを出さずに進むよりは、ここで止める。
    #[error("{0}")]
    NoEnvironment(String),
    /// ファイルやプロセスの操作が失敗した (Python 版の `OSError`)。
    #[error("{0}")]
    Io(String),
}

impl From<EngineError> for EngineFault {
    fn from(e: EngineError) -> Self {
        EngineFault::Setup(e.0)
    }
}
