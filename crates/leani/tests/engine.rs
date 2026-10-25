//! `Engine` と `Repl` を直接呼ぶテスト。1 テストに数秒かかる。

#![allow(
    non_snake_case,
    uncommon_codepoints,
    confusable_idents,
    mixed_script_confusables
)]

mod common;

use common::sandbox;
use leani::engine::Engine;
use leani_core::types::EngineFault;
use leani_read::config::EnvConfig;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use story::story;

/// 表示を受け取るだけの出口。エンジンを用意するときの表示はテストでは確かめない。
fn sink() -> Vec<u8> {
    Vec::new()
}

fn died_message(r: Result<impl std::fmt::Debug, EngineFault>) -> String {
    match r {
        Err(EngineFault::Died(why)) => why,
        other => panic!("Died にならない: {other:?}"),
    }
}

mod 起動時の確認 {
    //! boot と、エンジンの異常終了の報告。
    use super::*;

    #[story("G4")]
    #[test]
    fn 解決できない_import_では起動を中止する() {
        let sb = sandbox();
        let cfg = EnvConfig::make(
            &sb.ctx,
            "bogus",
            None,
            &["NoSuchModuleXYZ".to_string()],
            None,
            None,
        );
        let mut eng = Engine::new(&sb.ctx, cfg, &mut sink()).expect("起動できない");
        let why = died_message(eng.boot());
        assert!(why.contains("import に失敗した"), "{why}");
    }

    #[story("G4")]
    #[test]
    fn エンジンが異常終了したら終了シグナルや終了コードを報告に含める() {
        let sb = sandbox();
        let cfg = EnvConfig::make(&sb.ctx, "bogus", None, &[], None, None);
        let mut eng = Engine::new(&sb.ctx, cfg, &mut sink()).expect("起動できない");
        let pid = eng.pid().expect("プロセスが無い");
        kill(Pid::from_raw(pid.cast_signed()), Signal::SIGKILL).expect("kill");
        let why = died_message(eng.send_cmd("def after_kill := 1", true));
        assert!(why.contains("SIGKILL"), "{why}");
    }
}
