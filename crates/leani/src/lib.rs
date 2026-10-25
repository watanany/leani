//! leani: Lean 4 の対話 REPL。
//!
//! Python 版 (`src/leani/`) から移植している途中。モジュールの分け方は、
//! Python 版のラベル (純粋、読み取り、副作用、定数) を crate の境界で守る。
//!
//! ```text
//! leani-core   純粋と定数
//! leani-read   読み取り
//! leani        副作用 (この crate)
//! ```
//!
//! | モジュール | 内容                                                      |
//! |------------|-----------------------------------------------------------|
//! | `boot`     | Lean のバージョンの決定、エンジンの clone とビルド、lake env |
//! | `engine`   | repl のプロセスとのやりとり、環境と宣言の管理、再起動と replay  |
//! | `show`     | 表示を出す (`Output` の端末の実装、メッセージの整形)             |
//! | `repl`     | 入力を 1 行ずつ受け取り、エンジンに送って結果を表示する          |

pub mod boot;
pub mod engine;
pub mod repl;
pub mod show;
