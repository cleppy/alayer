//! Task 10: 5xx ve kısa gövde retry. 403 retry edilmez (single_connection testi).

use layer_engine_api::{EngineEvent, FailureReason, HttpEngine, HttpTaskSpec};
use layer_engine_http::ReqwestHttpEngine;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

fn spec(url: String, dest: std::path::PathBuf) -> HttpTaskSpec {
    HttpTaskSpec {
        task_id: "r".into(),
        url,
        headers: vec![],
        dest_dir: dest,
        file_name: "f.bin".into(),
        max_segments: 1,
        speed_limit_bps: None,
        resume: None,
    }
}

/// İlk `fail_times` istek 503, sonrası 200 + gövde. İstek sayacı döner.
async fn spawn_flaky(body: Vec<u8>, fail_times: u32) -> (std::net::SocketAddr, Arc<AtomicU32>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicU32::new(0));
    let hits_c = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { break };
            let body = body.clone();
            let hits = hits_c.clone();
            tokio::spawn(async move {
                let n = hits.fetch_add(1, Ordering::SeqCst);
                let (r, mut w) = tokio::io::split(sock);
                let mut reader = BufReader::new(r);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 { return; }
                    if line == "\r\n" { break; }
                }
                if n < fail_times {
                    let _ = w.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                } else {
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    if w.write_all(head.as_bytes()).await.is_ok() {
                        let _ = w.write_all(&body).await;
                    }
                }
            });
        }
    });
    (addr, hits)
}

#[tokio::test]
async fn retries_503_then_succeeds() {
    let body = pattern(8_000);
    let (addr, hits) = spawn_flaky(body.clone(), 2).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = ReqwestHttpEngine::for_tests();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let spec = spec(format!("http://{addr}/f.bin"), dir.path().join("dl"));
    let res = engine.run(spec, tx, CancellationToken::new(), None).await;
    res.unwrap();
    let mut done = false;
    while let Some(ev) = rx.recv().await {
        if matches!(ev, EngineEvent::Done) {
            done = true;
        }
    }
    assert!(done);
    assert!(hits.load(Ordering::SeqCst) >= 3, "2 fail + 1 başarı");
    assert_eq!(std::fs::read(dir.path().join("dl/f.bin")).unwrap(), body);
}

#[tokio::test]
async fn exhausted_503_fails() {
    let (addr, hits) = spawn_flaky(pattern(100), 99).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = ReqwestHttpEngine::for_tests();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let spec = spec(format!("http://{addr}/f.bin"), dir.path().join("dl"));
    engine.run(spec, tx, CancellationToken::new(), None).await.unwrap();
    let mut failed = false;
    while let Some(ev) = rx.recv().await {
        if matches!(ev, EngineEvent::Failed(FailureReason::Http { status: 503, .. }) | EngineEvent::Failed(FailureReason::Network(_))) {
            failed = true;
        }
    }
    assert!(failed);
    assert!(hits.load(Ordering::SeqCst) <= 6, "sınırsız retry yok");
    assert!(!dir.path().join("dl/f.bin").exists());
}
