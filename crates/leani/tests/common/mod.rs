//! エンジンを使うテストの共通部分 (Python 版の `conftest.py` に当たる)。
//!
//! `spec` は `crates/leani/tests/*.rs` だけを読むので、このファイルのテストは
//! SPEC に載らない。ここにはテストを書かない。
//!
//! どのテストも Lean 本体だけを import する環境で実行する。特定の Lake プロジェクト
//! に依存しないので、このリポジトリの外に持ち出してもそのまま実行できる。

#![allow(dead_code)]

use leani_read::places::Ctx;
use std::fs;
use std::path::Path;

/// 設定、履歴、init を置く一時ディレクトリ。`Ctx` と同じ寿命で持つ。
pub struct Sandbox {
    pub ctx: Ctx,
    pub dir: tempfile::TempDir,
}

/// 環境変数は PATH と HOME などプロセスのものを使い、設定、履歴、init だけを
/// 一時ディレクトリに向ける。環境は core (既定) と wide の 2 つ。
///
/// Python 版は環境変数そのものを書き換えていたが、Rust 版の `Ctx` は値を持つだけ
/// なので、テストごとに別の `Ctx` を作れる。
pub fn sandbox() -> Sandbox {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    fs::write(
        &config,
        "default = \"core\"\n\n[env.core]\nimports = []\n\n[env.wide]\nimports = [\"Lean.Elab.Frontend\"]\nprompt = \"λ+> \"\n",
    )
    .expect("write config");
    let at = |p: &Path| p.to_string_lossy().into_owned();
    let vars = [
        ("LEANI_INIT", at(&dir.path().join("no-such-init.lean"))),
        ("LEANI_HISTORY", at(&dir.path().join("history"))),
        ("LEANI_CONFIG", at(&config)),
    ];
    let var = |k: &str| match vars.iter().find(|(name, _)| *name == k) {
        Some((_, v)) => Some(v.clone()),
        None if k.starts_with("LEANI_") => None,
        None => std::env::var(k).ok(),
    };
    let cwd = std::env::current_dir().expect("cwd");
    let ctx = Ctx::new(var, &at(&cwd), false);
    Sandbox { ctx, dir }
}
