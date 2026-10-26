//! leani の純粋な関数と定数。
//!
//! この crate はプロセス、ファイル、環境変数、ネットワークを使わない。入力だけで
//! 出力が決まるので、テストは入力と出力だけで書ける。禁じている関数と型は
//! `clippy.toml` に書いてある。
//!
//! ```text
//! types     送り方のラベル、入力の状態、証明モードの状態、エラー (純粋)
//! queries   エンジンに送る Lean のコードと、応答を読む正規表現 (定数)
//! abbrev    略記の表と展開、`:abbrev` の表示 (純粋)
//! width     端末で表示したときの文字の幅の表 (定数)
//! history   履歴ファイルの形式の読み書き (純粋)
//! pure      判定と整形 (純粋)
//! ```
//!
//! 位置は byte で扱う。Lean の `column` (コードポイント単位) との変換は `pure::offset`
//! だけで行う。表示の幅や切り詰め (`pure::clip`、`pure::span`) は文字の数で数える。

pub mod abbrev;
pub mod history;
pub mod pure;
pub mod queries;
pub mod types;
mod width;
