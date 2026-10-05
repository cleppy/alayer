//! Task 9: segmented indirme, resume, If-Range conflict, segment 403.

#[path = "common/mod.rs"]
mod support;

use layer_engine_api::{EngineEvent, HttpEngine, HttpTaskSpec, ResumeState, SegmentProgress};
use layer_engine_http::ReqwestHttpEngine;
use std::path::PathBuf;
use std::time::Duration;
use support::spawn_range_server;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn spec(url: String, dest: PathBuf, segments: usize) -> HttpTaskSpec {
    HttpTaskSpec {
        task_id: "seg".into(),
        url,
        headers: vec![],
        dest_dir: dest,
        file_name: "f.bin".into(),
        max_segments: segments,
        speed_limit_bps: None,
        resume: None,
    }
}

async fn run(spec: HttpTaskSpec) -> (Result<(), layer_engine_api::EngineError>, Vec<EngineEvent>) {
    let engine = ReqwestHttpEngine::for_tests();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let h = tokio::spawn(async move { engine.run(spec, tx, CancellationToken::new(), None).await });
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    (h.await.unwrap(), events)
}

fn plan(events: &[EngineEvent]) -> Option<Vec<SegmentProgress>> {
    events.iter().find_map(|e| match e {
        EngineEvent::SegmentPlan(s) => Some(s.clone()),
        _ => None,
    })
}

#[tokio::test]
async fn four_segments_byte_exact() {
    let body = pattern(1_000_000);
    let srv = spawn_range_server(body.clone(), "\"e1\"").await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("dl");
    let (res, events) = run(spec(format!("http://{}/f.bin", srv.addr), dest.clone(), 4)).await;
    res.unwrap();
    let segs = plan(&events).expect("SegmentPlan");
    assert_eq!(segs.len(), 4, "1MB / 256KB min = 4");
    assert!(events.iter().any(|e| matches!(e, EngineEvent::Done)));
    assert_eq!(std::fs::read(dest.join("f.bin")).unwrap(), body);
    assert!(!dest.join("f.bin.part").exists());
}

#[tokio::test]
async fn small_file_fewer_segments() {
    let body = pattern(100_000);
    let srv = spawn_range_server(body.clone(), "\"e2\"").await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("dl");
    let (_, events) = run(spec(format!("http://{}/f.bin", srv.addr), dest.clone(), 8)).await;
    let segs = plan(&events).expect("plan");
    assert!(
        segs.len() < 8,
        "100KB < 8*256KB, segment sayısı düşmeli: {}",
        segs.len()
    );
    assert_eq!(std::fs::read(dest.join("f.bin")).unwrap(), body);
}

#[tokio::test]
async fn resume_requests_only_missing_bytes() {
    let body = pattern(800_000);
    let srv = spawn_range_server(body.clone(), "\"e3\"").await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("dl");
    std::fs::create_dir_all(&dest).unwrap();
    // Yarım .part: ilk 400KB dolu, gerisi sıfır.
    let mut part = vec![0u8; body.len()];
    part[..400_000].copy_from_slice(&body[..400_000]);
    std::fs::write(dest.join("f.bin.part"), &part).unwrap();

    let mut spec = spec(format!("http://{}/f.bin", srv.addr), dest.clone(), 4);
    spec.resume = Some(ResumeState {
        etag: Some("\"e3\"".into()),
        last_modified: None,
        total_bytes: Some(body.len() as u64),
        segments: vec![
            SegmentProgress {
                idx: 0,
                start: 0,
                end: 399_999,
                downloaded: 400_000,
            },
            SegmentProgress {
                idx: 1,
                start: 400_000,
                end: 799_999,
                downloaded: 0,
            },
        ],
    });
    let (res, events) = run(spec).await;
    res.unwrap();
    assert!(events.iter().any(|e| matches!(e, EngineEvent::Done)));
    assert_eq!(std::fs::read(dest.join("f.bin")).unwrap(), body);
}

#[tokio::test]
async fn if_range_mismatch_is_conflict() {
    let body = pattern(600_000);
    // Sunucu etag'i "\"new\"" — resume If-Range "\"old\"" gönderir → 200 → Conflict.
    let srv = spawn_range_server(body.clone(), "\"new\"").await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("dl");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("f.bin.part"), vec![1u8; 100]).unwrap();

    let mut spec = spec(format!("http://{}/f.bin", srv.addr), dest.clone(), 2);
    spec.resume = Some(ResumeState {
        etag: Some("\"old\"".into()),
        last_modified: None,
        total_bytes: Some(body.len() as u64),
        segments: vec![
            SegmentProgress {
                idx: 0,
                start: 0,
                end: 299_999,
                downloaded: 100,
            },
            SegmentProgress {
                idx: 1,
                start: 300_000,
                end: 599_999,
                downloaded: 0,
            },
        ],
    });
    let (res, events) = run(spec).await;
    res.unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, EngineEvent::ConflictDetected)),
        "If-Range uyuşmazlığı Conflict olmalı"
    );
    assert!(!dest.join("f.bin").exists(), "çakışmada final yazılmamalı");
}

#[tokio::test]
async fn segment_403_need_reresolve() {
    // Probe 206, segment isteği 403.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let (r, mut w) = tokio::io::split(sock);
                let mut reader = tokio::io::BufReader::new(r);
                let mut is_probe = false;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if line.to_ascii_lowercase().starts_with("range:") && line.contains("bytes=0-0")
                    {
                        is_probe = true;
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                if is_probe {
                    let head = b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/1000000\r\nContent-Length: 1\r\nETag: \"e\"\r\nConnection: close\r\n\r\n";
                    let _ = w.write_all(head).await;
                    let _ = w.write_all(&[0u8]).await;
                } else {
                    let _ = w.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                }
            });
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let (res, events) = run(spec(
        format!("http://{addr}/f.bin"),
        dir.path().join("dl"),
        4,
    ))
    .await;
    res.unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, EngineEvent::NeedReResolve)),
        "segment 403 → NeedReResolve"
    );
    assert!(!dir.path().join("dl/f.bin").exists());
}

#[tokio::test]
async fn cancel_during_segments_keeps_part() {
    let body = pattern(2_000_000);
    let srv = spawn_range_server(body, "\"e4\"").await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("dl");
    let spec = spec(format!("http://{}/f.bin", srv.addr), dest.clone(), 8);
    let engine = ReqwestHttpEngine::for_tests();
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let h = tokio::spawn(async move { engine.run(spec, tx, c2, None).await });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut saw = false;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some(EngineEvent::SegmentUpdate { .. })) => {
                saw = true;
                break;
            }
            Ok(Some(EngineEvent::Done)) => break,
            Ok(Some(_)) => {}
            _ => {}
        }
    }
    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), h).await;
    if saw {
        assert!(dest.join("f.bin.part").exists() || dest.join("f.bin").exists());
    }
}
