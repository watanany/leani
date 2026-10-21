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

/// エンジンを用意できない。起動時なら起動を中止する。起動後なら REPL は続ける。
#[derive(Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct EngineError(pub String);

/// 設定が読めない、または指定された環境が無い。
#[derive(Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct ConfigError(pub String);
