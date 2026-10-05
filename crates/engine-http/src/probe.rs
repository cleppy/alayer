//! Probe: Range desteği, toplam boyut ve doğrulayıcılar (ETag / Last-Modified).
//!
//! Tek istek: `GET` + `Range: bytes=0-0`.
//! - 206 + `Content-Range: bytes 0-0/N` → `range_ok`, toplam = N.
//! - 200 → sunucu Range'i yok saydı (Accept-Ranges yalan da olsa). `range_ok=false`.
//!   Gövde düşürülür; indirme ayrı istekte başlar (ilk byte'ı saklamak resume'u bozar).
//! - 416 → Range anlamsız (boş/bilinmeyen). `range_ok=false`.
//! - 403/410 → bu probe'un işi değil. CDN Range reddi 403 olabilir; "URL öldü"
//!   kararı yalnız asıl indirme gövdesinde verilir (`NeedReResolve`).
//! - 5xx / ağ → geçici. Çağıran retry eder.

use crate::http::{apply_headers, is_expired_url};
use layer_engine_api::{EngineError, HttpTaskSpec};
use reqwest::StatusCode;

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub total_bytes: Option<u64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub range_ok: bool,
}

pub enum ProbeFail {
    /// Geçici: 5xx veya bağlantı/timeout. Retry edilir.
    Transient {
        status: Option<u16>,
        message: String,
    },
    /// Kalıcı HTTP (4xx, 403/410 hariç).
    Http {
        status: u16,
        message: String,
    },
    Io(String),
}

fn header_str(resp: &reqwest::Response, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// `Content-Range: bytes 0-0/12345` veya `bytes */12345` → toplam.
fn parse_total(content_range: &str) -> Option<u64> {
    let rest = content_range.strip_prefix("bytes ")?;
    let total = rest.split('/').nth(1)?.trim();
    if total == "*" {
        return None;
    }
    total.parse().ok()
}

pub async fn probe(
    client: &reqwest::Client,
    spec: &HttpTaskSpec,
) -> Result<ProbeResult, ProbeFail> {
    let mut req = client
        .get(&spec.url)
        .header(reqwest::header::RANGE, "bytes=0-0");
    req = apply_headers(req, &spec.headers);
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            return Err(ProbeFail::Transient {
                status: None,
                message: e.to_string(),
            })
        }
    };

    let status = resp.status();
    if is_expired_url(status) || status == StatusCode::RANGE_NOT_SATISFIABLE {
        // 403/410/416: Range bu kaynakta işe yaramıyor. Toplamı Content-Length'ten al.
        return Ok(single_connection_probe(&resp));
    }
    if status.is_server_error() {
        return Err(ProbeFail::Transient {
            status: Some(status.as_u16()),
            message: status.canonical_reason().unwrap_or("server error").into(),
        });
    }
    if status.is_client_error() {
        return Err(ProbeFail::Http {
            status: status.as_u16(),
            message: status.canonical_reason().unwrap_or("client error").into(),
        });
    }

    if status == StatusCode::PARTIAL_CONTENT {
        let total = header_str(&resp, "content-range")
            .as_deref()
            .and_then(parse_total);
        // 206 ama toplam yok → segmentlenemez, tek akışa düş.
        let range_ok = total.is_some();
        return Ok(ProbeResult {
            total_bytes: total.or_else(|| resp.content_length()),
            etag: header_str(&resp, "etag"),
            last_modified: header_str(&resp, "last-modified"),
            range_ok,
        });
    }

    // 200 (veya 3xx sonrası 200): Range yok sayıldı.
    Ok(single_connection_probe(&resp))
}

fn single_connection_probe(resp: &reqwest::Response) -> ProbeResult {
    ProbeResult {
        total_bytes: resp.content_length(),
        etag: header_str(resp, "etag"),
        last_modified: header_str(resp, "last-modified"),
        range_ok: false,
    }
}

impl From<ProbeFail> for EngineError {
    fn from(f: ProbeFail) -> Self {
        match f {
            ProbeFail::Io(m) => EngineError::Internal(m),
            ProbeFail::Transient { message, .. } | ProbeFail::Http { message, .. } => {
                EngineError::Internal(message)
            }
        }
    }
}
