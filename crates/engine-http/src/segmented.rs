//! Çok segmentli indirme. Tek `.part`, her segment kendi offset'ine yazar.
//!
//! Plan: toplam / max_segments, minimum segment 256KB. Resume varsa sınırlar
//! korunur, `downloaded` kadar atlanır.
//! If-Range 200 → Conflict, parçalar birleştirilmez.
//! Segment 403/410 → NeedReResolve, diğer segmentler durur.

use crate::http::{apply_headers, is_expired_url, sleep_backoff, RetryPolicy};
use crate::probe::ProbeResult;
use layer_engine_api::{
    acquire_all, ByteBudget, EngineError, EngineEvent, HttpTaskSpec, ResumeState, SegmentProgress,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const MIN_SEGMENT: u64 = 256 * 1024;

pub enum SegmentedOutcome {
    Completed,
    Cancelled,
    Conflict,
    NeedReResolve,
    Failed(layer_engine_api::FailureReason),
}

pub fn plan(total: u64, max_segments: usize, resume: Option<&ResumeState>) -> Vec<SegmentProgress> {
    if let Some(r) = resume {
        if !r.segments.is_empty() && r.total_bytes == Some(total) {
            return r.segments.clone();
        }
    }
    if total == 0 {
        return vec![];
    }
    let max = max_segments.max(1);
    let n = if total < MIN_SEGMENT {
        1
    } else {
        max.min((total.div_ceil(MIN_SEGMENT) as usize).max(1))
    };
    let mut segs = Vec::with_capacity(n);
    let base = total / n as u64;
    let mut start = 0u64;
    for i in 0..n {
        let end = if i + 1 == n {
            total - 1
        } else {
            start + base - 1
        };
        segs.push(SegmentProgress {
            idx: i as u32,
            start,
            end,
            downloaded: 0,
        });
        start = end + 1;
    }
    segs
}

#[allow(clippy::too_many_arguments)]
pub async fn run_segmented(
    client: &reqwest::Client,
    spec: &HttpTaskSpec,
    probe: &ProbeResult,
    segments: Vec<SegmentProgress>,
    events: &mpsc::UnboundedSender<EngineEvent>,
    cancel: &CancellationToken,
    budgets: &[Arc<ByteBudget>],
    policy: &RetryPolicy,
) -> Result<SegmentedOutcome, EngineError> {
    let total = probe
        .total_bytes
        .ok_or_else(|| EngineError::InvalidSpec("segmented requires known total".into()))?;
    std::fs::create_dir_all(&spec.dest_dir)?;
    let final_p = spec.dest_dir.join(&spec.file_name);
    if final_p.exists() {
        return Ok(SegmentedOutcome::Failed(
            layer_engine_api::FailureReason::Io(format!("hedef zaten var: {}", final_p.display())),
        ));
    }
    let part = spec.dest_dir.join(format!("{}.part", spec.file_name));
    {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&part)?;
        f.set_len(total)?;
    }
    let _ = events.send(EngineEvent::SegmentPlan(segments.clone()));

    let file = Arc::new(tokio::sync::Mutex::new(
        tokio::fs::OpenOptions::new()
            .write(true)
            .open(&part)
            .await?,
    ));
    let stop = Arc::new(AtomicBool::new(false));
    let conflict = Arc::new(AtomicBool::new(false));
    let expired = Arc::new(AtomicBool::new(false));
    let downloaded = Arc::new(AtomicU64::new(segments.iter().map(|s| s.downloaded).sum()));
    let fail_msg: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));

    let mut joins = Vec::new();
    for seg in segments {
        if seg.downloaded >= seg.byte_len() {
            continue;
        }
        let client = client.clone();
        let spec = spec.clone();
        let probe = probe.clone();
        let events = events.clone();
        let cancel = cancel.clone();
        let budgets = budgets.to_vec();
        let policy = *policy;
        let file = file.clone();
        let stop = stop.clone();
        let conflict = conflict.clone();
        let expired = expired.clone();
        let downloaded = downloaded.clone();
        let fail_msg = fail_msg.clone();
        joins.push(tokio::spawn(async move {
            download_segment(
                &client,
                &spec,
                &probe,
                seg,
                &events,
                &cancel,
                &budgets,
                &policy,
                &file,
                &stop,
                &conflict,
                &expired,
                &downloaded,
                &fail_msg,
            )
            .await
        }));
    }

    for j in joins {
        let _ = j.await;
    }

    if cancel.is_cancelled()
        || stop.load(Ordering::SeqCst)
            && !conflict.load(Ordering::SeqCst)
            && !expired.load(Ordering::SeqCst)
            && fail_msg.lock().unwrap().is_none()
    {
        // cancel veya temiz duruş
    }
    if conflict.load(Ordering::SeqCst) {
        return Ok(SegmentedOutcome::Conflict);
    }
    if expired.load(Ordering::SeqCst) {
        return Ok(SegmentedOutcome::NeedReResolve);
    }
    if cancel.is_cancelled() {
        return Ok(SegmentedOutcome::Cancelled);
    }
    if let Some(msg) = fail_msg.lock().unwrap().clone() {
        return Ok(SegmentedOutcome::Failed(
            layer_engine_api::FailureReason::Network(msg),
        ));
    }

    drop(file);
    if let Err(e) = std::fs::rename(&part, &final_p) {
        return Ok(SegmentedOutcome::Failed(
            layer_engine_api::FailureReason::Io(e.to_string()),
        ));
    }
    Ok(SegmentedOutcome::Completed)
}

#[allow(clippy::too_many_arguments)]
async fn download_segment(
    client: &reqwest::Client,
    spec: &HttpTaskSpec,
    probe: &ProbeResult,
    seg: SegmentProgress,
    events: &mpsc::UnboundedSender<EngineEvent>,
    cancel: &CancellationToken,
    budgets: &[Arc<ByteBudget>],
    policy: &RetryPolicy,
    file: &Arc<tokio::sync::Mutex<tokio::fs::File>>,
    stop: &AtomicBool,
    conflict: &AtomicBool,
    expired: &AtomicBool,
    downloaded_total: &AtomicU64,
    fail_msg: &std::sync::Mutex<Option<String>>,
) {
    let mut got = seg.downloaded;
    let need = seg.byte_len();
    let mut attempt = 0u32;
    while got < need && !stop.load(Ordering::SeqCst) && !cancel.is_cancelled() {
        let start = seg.start + got;
        let end = seg.end;
        match fetch_range(
            client,
            spec,
            probe,
            start,
            end,
            file,
            cancel,
            budgets,
            downloaded_total,
            events,
        )
        .await
        {
            Ok(n) => {
                got += n;
                let _ = events.send(EngineEvent::SegmentUpdate {
                    idx: seg.idx,
                    downloaded: got,
                });
                if n == 0 && got < need {
                    attempt += 1;
                    if attempt >= policy.max_attempts {
                        stop.store(true, Ordering::SeqCst);
                        *fail_msg.lock().unwrap() =
                            Some(format!("segment {} kısa kapandı", seg.idx));
                        return;
                    }
                    if !sleep_backoff(attempt, policy, cancel).await {
                        return;
                    }
                } else {
                    attempt = 0;
                }
            }
            Err(SegErr::Conflict) => {
                conflict.store(true, Ordering::SeqCst);
                stop.store(true, Ordering::SeqCst);
                return;
            }
            Err(SegErr::Expired) => {
                expired.store(true, Ordering::SeqCst);
                stop.store(true, Ordering::SeqCst);
                return;
            }
            Err(SegErr::Transient(msg)) => {
                attempt += 1;
                if attempt >= policy.max_attempts {
                    stop.store(true, Ordering::SeqCst);
                    *fail_msg.lock().unwrap() = Some(msg);
                    return;
                }
                if !sleep_backoff(attempt, policy, cancel).await {
                    return;
                }
            }
        }
    }
}

enum SegErr {
    Conflict,
    Expired,
    Transient(String),
}

#[allow(clippy::too_many_arguments)]
async fn fetch_range(
    client: &reqwest::Client,
    spec: &HttpTaskSpec,
    probe: &ProbeResult,
    start: u64,
    end: u64,
    file: &Arc<tokio::sync::Mutex<tokio::fs::File>>,
    cancel: &CancellationToken,
    budgets: &[Arc<ByteBudget>],
    downloaded_total: &AtomicU64,
    events: &mpsc::UnboundedSender<EngineEvent>,
) -> Result<u64, SegErr> {
    let mut req = client
        .get(&spec.url)
        .header(reqwest::header::RANGE, format!("bytes={start}-{end}"));
    if let Some(v) = probe.etag.as_deref().or(probe.last_modified.as_deref()) {
        req = req.header(reqwest::header::IF_RANGE, v);
    }
    req = apply_headers(req, &spec.headers);
    let resp = req
        .send()
        .await
        .map_err(|e| SegErr::Transient(e.to_string()))?;
    let status = resp.status();
    if is_expired_url(status) {
        return Err(SegErr::Expired);
    }
    if status == reqwest::StatusCode::OK {
        return Err(SegErr::Conflict);
    }
    if !status.is_success() {
        return Err(SegErr::Transient(format!("HTTP {}", status.as_u16())));
    }

    let mut stream = resp.bytes_stream();
    use futures::StreamExt;
    let mut wrote = 0u64;
    let mut cursor = start;
    let mut last = tokio::time::Instant::now();
    while let Some(chunk) = stream.next().await {
        if cancel.is_cancelled() {
            return Ok(wrote);
        }
        let chunk = chunk.map_err(|e| SegErr::Transient(e.to_string()))?;
        if !budgets.is_empty() {
            acquire_all(budgets, chunk.len() as u64, cancel).await;
        }
        {
            let mut f = file.lock().await;
            f.seek(std::io::SeekFrom::Start(cursor))
                .await
                .map_err(|e| SegErr::Transient(e.to_string()))?;
            f.write_all(&chunk)
                .await
                .map_err(|e| SegErr::Transient(e.to_string()))?;
        }
        cursor += chunk.len() as u64;
        wrote += chunk.len() as u64;
        let total_now =
            downloaded_total.fetch_add(chunk.len() as u64, Ordering::Relaxed) + chunk.len() as u64;
        if last.elapsed() >= std::time::Duration::from_millis(250) {
            let _ = events.send(EngineEvent::Progress(layer_engine_api::Progress {
                downloaded_bytes: total_now,
                total_bytes: probe.total_bytes,
                speed_bps: 0.0,
            }));
            last = tokio::time::Instant::now();
        }
        if cursor > end {
            break;
        }
    }
    Ok(wrote)
}
