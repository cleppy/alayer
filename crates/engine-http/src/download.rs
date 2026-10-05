//! Tek bağlantılı indirme. Range yok, toplam bilinmiyor, veya segment sayısı 1.
//!
//! `.part` = yalnız byte. Durum (ETag, offset) core'un DB'sinde.
//! Resume: `spec.resume` içindeki tek segmentin `downloaded` değeri kadar atlanır
//! (`Range: bytes=N-`). Kaynak değişmişse (If-Range → 200) Conflict.

use crate::http::{apply_headers, is_expired_url, sleep_backoff, RetryPolicy};
use crate::probe::ProbeResult;
use layer_engine_api::{
    acquire_all, ByteBudget, EngineError, EngineEvent, FailureReason, HttpTaskSpec, Progress,
    SegmentProgress,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

pub enum SingleOutcome {
    Completed,
    Cancelled,
    Conflict,
    NeedReResolve,
    Failed(FailureReason),
}

fn part_path(spec: &HttpTaskSpec) -> PathBuf {
    spec.dest_dir.join(format!("{}.part", spec.file_name))
}

fn final_path(spec: &HttpTaskSpec) -> PathBuf {
    spec.dest_dir.join(&spec.file_name)
}

/// Resume offset: tek segment varsa onun `downloaded`'ı, yoksa 0.
fn resume_offset(spec: &HttpTaskSpec) -> u64 {
    spec.resume
        .as_ref()
        .and_then(|r| r.segments.first())
        .map(|s| s.downloaded)
        .unwrap_or(0)
}

pub async fn run_single(
    client: &reqwest::Client,
    spec: &HttpTaskSpec,
    probe: &ProbeResult,
    events: &tokio::sync::mpsc::UnboundedSender<EngineEvent>,
    cancel: &CancellationToken,
    budgets: &[Arc<ByteBudget>],
    policy: &RetryPolicy,
) -> Result<SingleOutcome, EngineError> {
    std::fs::create_dir_all(&spec.dest_dir)?;
    let final_p = final_path(spec);
    if final_p.exists() {
        return Ok(SingleOutcome::Failed(FailureReason::Io(format!(
            "hedef zaten var: {}",
            final_p.display()
        ))));
    }

    let offset = resume_offset(spec);
    let part = part_path(spec);
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(offset == 0)
        .open(&part)
        .await?;
    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset)).await?;
    }

    // Her deneme dosyayı baştan (veya resume offset'inden) yazar.
    // Kısmi gövde sonrası retry, offset'i dosya uzunluğundan okur.
    let mut attempt = 0u32;
    loop {
        if cancel.is_cancelled() {
            return Ok(SingleOutcome::Cancelled);
        }
        let start_at = file.metadata().await.map(|m| m.len()).unwrap_or(offset);
        file.seek(std::io::SeekFrom::Start(start_at)).await?;
        match fetch_body(
            client, spec, probe, start_at, &mut file, events, cancel, budgets,
        )
        .await
        {
            Ok(SingleOutcome::Completed) => {
                drop(file);
                if let Err(e) = std::fs::rename(&part, &final_p) {
                    return Ok(SingleOutcome::Failed(FailureReason::Io(e.to_string())));
                }
                return Ok(SingleOutcome::Completed);
            }
            Ok(other) => return Ok(other),
            Err(FetchErr::Expired) => return Ok(SingleOutcome::NeedReResolve),
            Err(FetchErr::Conflict) => return Ok(SingleOutcome::Conflict),
            Err(FetchErr::Transient(msg)) => {
                attempt += 1;
                if attempt >= policy.max_attempts {
                    return Ok(SingleOutcome::Failed(FailureReason::Network(msg)));
                }
                if !sleep_backoff(attempt, policy, cancel).await {
                    return Ok(SingleOutcome::Cancelled);
                }
            }
            Err(FetchErr::Http { status, message }) => {
                return Ok(SingleOutcome::Failed(FailureReason::Http {
                    status,
                    message,
                }));
            }
            Err(FetchErr::Io(e)) => return Err(e.into()),
        }
    }
}

enum FetchErr {
    Expired,
    Conflict,
    Transient(String),
    Http { status: u16, message: String },
    Io(std::io::Error),
}

#[allow(clippy::too_many_arguments)]
async fn fetch_body(
    client: &reqwest::Client,
    spec: &HttpTaskSpec,
    probe: &ProbeResult,
    start_at: u64,
    file: &mut tokio::fs::File,
    events: &tokio::sync::mpsc::UnboundedSender<EngineEvent>,
    cancel: &CancellationToken,
    budgets: &[Arc<ByteBudget>],
) -> Result<SingleOutcome, FetchErr> {
    let mut req = client.get(&spec.url);
    if start_at > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={start_at}-"));
        if let Some(v) = probe.etag.as_deref().or(probe.last_modified.as_deref()) {
            req = req.header(reqwest::header::IF_RANGE, v);
        }
    }
    req = apply_headers(req, &spec.headers);

    let resp = req
        .send()
        .await
        .map_err(|e| FetchErr::Transient(e.to_string()))?;
    let status = resp.status();

    if is_expired_url(status) {
        return Err(FetchErr::Expired);
    }
    // If-Range 200: kaynak değişti, parçayı birleştirme.
    if start_at > 0 && status == reqwest::StatusCode::OK {
        return Err(FetchErr::Conflict);
    }
    if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(FetchErr::Transient(format!("HTTP {}", status.as_u16())));
    }
    if !status.is_success() {
        return Err(FetchErr::Http {
            status: status.as_u16(),
            message: status.canonical_reason().unwrap_or("http error").into(),
        });
    }

    let mut stream = resp.bytes_stream();
    use futures::StreamExt;
    let mut downloaded = start_at;
    let mut last_emit = tokio::time::Instant::now();
    let started = tokio::time::Instant::now();

    while let Some(chunk) = stream.next().await {
        if cancel.is_cancelled() {
            let _ = file.flush().await;
            return Ok(SingleOutcome::Cancelled);
        }
        let chunk = chunk.map_err(|e| FetchErr::Transient(e.to_string()))?;
        if !budgets.is_empty() {
            acquire_all(budgets, chunk.len() as u64, cancel).await;
            if cancel.is_cancelled() {
                let _ = file.flush().await;
                return Ok(SingleOutcome::Cancelled);
            }
        }
        file.write_all(&chunk).await.map_err(FetchErr::Io)?;
        downloaded += chunk.len() as u64;

        if last_emit.elapsed() >= std::time::Duration::from_millis(250) {
            let elapsed = started.elapsed().as_secs_f64().max(0.001);
            let _ = events.send(EngineEvent::Progress(Progress {
                downloaded_bytes: downloaded,
                total_bytes: probe.total_bytes,
                speed_bps: (downloaded.saturating_sub(start_at) as f64) / elapsed,
            }));
            last_emit = tokio::time::Instant::now();
        }
    }
    file.flush().await.map_err(FetchErr::Io)?;

    // Beklenen toplam varsa ve eksiksek, bu geçici bir kesilmedir.
    if let Some(total) = probe.total_bytes {
        if downloaded < total {
            return Err(FetchErr::Transient(format!(
                "eksik gövde: {downloaded}/{total}"
            )));
        }
    }
    let _ = events.send(EngineEvent::Progress(Progress {
        downloaded_bytes: downloaded,
        total_bytes: probe.total_bytes,
        speed_bps: 0.0,
    }));
    // Tek segment planı: core resume için downloaded'ı bilir.
    if let Some(total) = probe.total_bytes {
        let _ = events.send(EngineEvent::SegmentPlan(vec![SegmentProgress {
            idx: 0,
            start: 0,
            end: total.saturating_sub(1),
            downloaded,
        }]));
    }
    Ok(SingleOutcome::Completed)
}
