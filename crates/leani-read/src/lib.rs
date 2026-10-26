//! leani の読み取りの関数。
//!
//! 環境変数、カレントディレクトリ、設定ファイル、lakefile を読むが、何も書き換えない。
//! プロセスとネットワークも使わない。禁じている関数と型は `clippy.toml` に書いてある。
//!
//! ```text
//! places    パスと、環境変数で決まる値 (Ctx)
//! config    起動する環境を決める。設定ファイルと lakefile を読む
//! complete  Tab 補完のうち、ファイルと PATH を読む候補 (パスとコマンド)
//! args      コマンドラインの引数を読む
//! ```

pub mod args;
pub mod complete;
pub mod config;
pub mod places;
