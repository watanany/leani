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
