//! `:env` で環境を切り替えるときの手順。
//!
//! `:env` は、切り替え先のエンジンを起動する前に、今のエンジンを終了させる。
//! 切り替え先の起動が失敗するか中断されたら、元の環境でエンジンを起動し直す。
//! 元の環境に戻る起動も失敗したら、環境を失う。
//!
//! このモジュールは、起動の結果ごとに次に何をするかだけを決める ([`on_boot`])。
//! エンジンの起動と終了、表示、宣言の再実行は `leani` の `Repl::cmd_env` が行う。
//! 遷移が 1 つの `match` にまとまっているので、どの状態でどの結果が来たら何をするかを
//! ここだけで読める。
//!
//! この crate はエンジンを扱わないので、型を引数にしてある。`C` は環境の設定、
//! `E` は起動したエンジン、`D` は起動に失敗したエンジンとその理由である。

/// エンジンを起動した結果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Booted<E, D> {
    /// 起動した。
    Ok(E),
    /// Ctrl-C で中断した。
    Interrupted(D),
    /// 失敗した。
    Failed(D),
}

/// 切り替えの途中で、エンジンを起動しているときの状態。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Switch<C> {
    /// 切り替え先 `to` でエンジンを起動している。失敗したら `back` に戻る。
    Target { to: C, back: C },
    /// 切り替え先の起動に失敗したので、元の環境 `back` でエンジンを起動している。
    Back { back: C },
}

impl<C> Switch<C> {
    /// 今起動している環境。
    pub fn booting(&self) -> &C {
        match self {
            Switch::Target { to, .. } => to,
            Switch::Back { back } => back,
        }
    }
}

/// 起動の結果を受けて、次にすること。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next<C, E, D> {
    /// 起動したエンジンを今のエンジンにする。
    Install(E),
    /// 起動に失敗したエンジン `failed` を捨て、`next` の環境でエンジンを起動する。
    GoBack { failed: D, next: Switch<C> },
    /// 元の環境に戻る起動を中断した。エンジンが無いまま続ける。
    Suspend(D),
    /// 元の環境に戻る起動も失敗した。環境を失う。
    Lose(D),
}

/// 今の状態 `now` で起動の結果 `got` を受けたときに、次にすることを返す。
pub fn on_boot<C, E, D>(now: Switch<C>, got: Booted<E, D>) -> Next<C, E, D> {
    match (now, got) {
        (_, Booted::Ok(eng)) => Next::Install(eng),
        (Switch::Target { back, .. }, Booted::Interrupted(failed) | Booted::Failed(failed)) => {
            Next::GoBack {
                failed,
                next: Switch::Back { back },
            }
        }
        (Switch::Back { .. }, Booted::Interrupted(failed)) => Next::Suspend(failed),
        (Switch::Back { .. }, Booted::Failed(failed)) => Next::Lose(failed),
    }
}
