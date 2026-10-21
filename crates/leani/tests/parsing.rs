//! 純粋関数と読み取りのテスト。プロセスも端末も使わない。

#![allow(
    non_snake_case,
    uncommon_codepoints,
    confusable_idents,
    mixed_script_confusables
)]

use story::story;

/// SPEC.rust.md はテスト名から生成する (crates/spec)。手で書き足す場所を
/// 作らない代わりに、生成し忘れをこのテストで検出する。
mod describe_性質一覧 {
    use super::*;

    #[story("X1")]
    #[test]
    fn it_SPEC_md_がテストとそろっている() {
        if let Err(why) = spec::check(&spec::root()) {
            panic!("{why}");
        }
    }
}
