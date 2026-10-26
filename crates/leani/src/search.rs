//! 外部サービスに問い合わせる (副作用)。
//!
//! loogle で定理を検索する。エンジンとは関係なく、loogle は JSON だけを返す。整形は
//! `leani_core::pure::loogle_text` にある。leani が自分で HTTP リクエストを送るのは
//! このモジュールだけで、失敗はすべて `SearchError` にまとめる (ネットワークの問題で
//! REPL を終了させないため)。
//!
//! `Repl` は `Search` を通して問い合わせる。テストでは、決めた応答を返す偽物に
//! 差し替える (Python 版の `mocker.patch.object(leani.repl, "loogle")`)。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use leani_core::pure::url_quote;
use leani_core::types::{Json, SearchError};
use leani_read::places::Ctx;

/// 定理を検索する。
pub trait Search: Send {
    fn loogle(&self, query: &str) -> Result<Json, SearchError>;
}

/// 本物の loogle。URL とタイムアウトは `Ctx` から取る。
pub struct Loogle {
    url: String,
    timeout: Duration,
    interrupted: Arc<AtomicBool>,
}

impl Loogle {
    pub fn new(ctx: &Ctx, timeout: Duration) -> Loogle {
        Loogle {
            url: ctx.loogle.clone(),
            timeout,
            interrupted: ctx.interrupted.clone(),
        }
    }
}

impl Search for Loogle {
    /// 別のスレッドで問い合わせ、待つあいだ Ctrl-C のフラグを確かめる。フラグが
    /// 立っていたら待つのをやめて、中断したことを `SearchError` で返す。問い合わせの
    /// スレッドは、応答かタイムアウトで自然に終わる。
    fn loogle(&self, query: &str) -> Result<Json, SearchError> {
        let (tx, rx) = std::sync::mpsc::channel();
        let (url, timeout, query) = (self.url.clone(), self.timeout, query.to_string());
        std::thread::spawn(move || {
            let _ = tx.send(loogle(&url, &query, timeout));
        });
        loop {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(got) => return got,
                Err(RecvTimeoutError::Timeout) if self.interrupted.swap(false, SeqCst) => {
                    return Err(SearchError("^C 中断した".to_string()));
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(SearchError("問い合わせのスレッドが終了した".to_string()));
                }
            }
        }
    }
}

/// loogle に 1 回問い合わせる。失敗したら `SearchError` を返す。
///
/// `url` は loogle の JSON の入口 (`Ctx::loogle`)。テストではローカルのサーバを渡す。
pub fn loogle(url: &str, query: &str, timeout: Duration) -> Result<Json, SearchError> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut resp = agent
        .get(format!("{url}?q={}", url_quote(query)))
        .call()
        .map_err(|e| SearchError(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(SearchError(format!(
            "{} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        )));
    }
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| SearchError(e.to_string()))?;
    let got: Json = serde_json::from_str(&text).map_err(|e| SearchError(e.to_string()))?;
    if got.is_object() {
        Ok(got)
    } else {
        Err(SearchError(
            "応答が JSON のオブジェクトではない".to_string(),
        ))
    }
}
