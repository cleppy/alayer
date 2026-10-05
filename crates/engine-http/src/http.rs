//! Paylaşılan HTTP yardımcıları: header devri, durum sınıflandırma, retry.

use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// yt-dlp `http_headers` aynen gider. Hop-by-hop başlıklar (Host, Content-Length,
/// Connection, Transfer-Encoding, Range) spec'ten gelse bile basılmaz — reqwest
/// bunları kendisi yönetir, üzerine yazmak isteği bozar.
const HOP_BY_HOP: &[&str] = &[
    "host",
    "content-length",
    "connection",
    "transfer-encoding",
    "range",
    "if-range",
    "accept-encoding",
];

pub fn apply_headers(
    mut req: reqwest::RequestBuilder,
    headers: &[(String, String)],
) -> reqwest::RequestBuilder {
    for (k, v) in headers {
        if HOP_BY_HOP.iter().any(|h| h.eq_ignore_ascii_case(k)) {
            continue;
        }
        req = req.header(k, v);
    }
    req
}

/// 403/410: URL öldü (googlevideo imzası vb.). Kör retry yasak.
pub fn is_expired_url(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::GONE
}

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base: Duration,
    pub cap: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base: Duration::from_millis(200),
            cap: Duration::from_secs(4),
        }
    }
}

/// Testlerde süreleri kısaltmak için. Üretim `Default` kullanır.
pub fn policy_for_tests() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 3,
        base: Duration::from_millis(20),
        cap: Duration::from_millis(80),
    }
}

pub async fn sleep_backoff(attempt: u32, policy: &RetryPolicy, cancel: &CancellationToken) -> bool {
    if cancel.is_cancelled() {
        return false;
    }
    let exp = policy.base.saturating_mul(1u32 << attempt.min(8));
    let wait = exp.min(policy.cap);
    tokio::select! {
        _ = tokio::time::sleep(wait) => true,
        _ = cancel.cancelled() => false,
    }
}
