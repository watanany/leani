//! `#[story("A1", ...)]`: テストが対象にするユーザーストーリー (`stories.toml`) を指定する。
//!
//! テストの中身にも分け方にも影響しない。マクロは関数をそのまま返す。
//! `crates/spec` が `SPEC.md` を生成するときに、この指定をソースから読み、
//! ID が `stories.toml` にあるかもそこで確かめる。1 つのテストが複数のストーリーを
//! 対象にしてよく、1 つのストーリーを複数のテストが対象にしてよい。

use proc_macro::TokenStream;

#[proc_macro_attribute]
pub fn story(_ids: TokenStream, item: TokenStream) -> TokenStream {
    item
}
