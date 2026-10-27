//! leani: Lean 4 の対話 REPL。
//!
//! crate は副作用の有無で分けてある。依存の向きは `leani-core` ← `leani-read` ← `leani`
//! だけにする。
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
//! | `history`  | 確定した入力を 1 件として保存する履歴                       |
//! | `search`   | 外部サービス (loogle) への問い合わせ                        |
//! | `show`     | 表示を出す (`Output` の端末の実装、メッセージの整形、pager)      |
//! | `repl`     | 入力を 1 行ずつ受け取り、エンジンに送って結果を表示する。Tab 補完 |
//! | `cli`      | 引数を読んで `Repl` を起動する。入力を読むループ。終了コード      |
//! | `term`     | 端末の行編集。略記の変換、Tab 補完、ブロック単位の履歴          |
//!
//! `main.rs` が `leani` コマンドで、環境変数と、入力の元 (`term` か stdin) を `cli` に
//! 渡す。

pub mod boot;
pub mod cli;
pub mod engine;
pub mod history;
pub mod repl;
pub mod search;
pub mod show;
pub mod term;
