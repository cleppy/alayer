//! Task 8: probe + tek bağlantılı indirme.
//!
//! Sözleşme (engine-http):
//! - Probe `Range: bytes=0-0` atar. 206 → range_ok + Content-Range toplamı.
//!   200 → range_ok=false (Accept-Ranges yalan da olsa). 416 → range yok.
//! - Probe 5xx/ağ hatası → retry (Task 10 politikası). Tükenince Failed.
//! - Probe 403/410 → NeedReResolve DEĞİL. CDN'ler Range'i reddedip 403 dönebilir;
//!   bu "URL öldü" değildir. Asıl indirmede 403/410 → NeedReResolve (kör retry yok).
//! - Cancel: yazılmış byte `.part`'ta kalır, final oluşmaz.
//! - Hız limiti: görev (`spec.speed_limit_bps`) ve global (`run` argümanı) birlikte;
//!   ikisi de varsa yavaş olan kazanır.

use layer_engine_api::{ByteBudget, EngineEvent, FailureReason, HttpEngine, HttpTaskSpec};
use layer_engine_http::ReqwestHttpEngine;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn body_pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn spec_for(url: String, dest: PathBuf) -> HttpTaskSpec {
    HttpTaskSpec {
        task_id: "t-test".into(),
        url,
        headers: vec![],
        dest_dir: dest,
        file_name: "f.bin".into(),
        max_segments: 8,
        speed_limit_bps: None,
        resume: None,
    }
}

async fn run_collect(
    engine: &ReqwestHttpEngine,
    spec: HttpTaskSpec,
    cancel: CancellationToken,
    global: Option<Arc<ByteBudget>>,
) -> (Result<(), layer_engine_api::EngineError>, Vec<EngineEvent>) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let engine = engine.clone();
    let h = tokio::spawn(async move { engine.run(spec, tx, cancel, global).await });
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    (h.await.unwrap(), events)
}

fn probed(events: &[EngineEvent]) -> Option<(Option<u64>, Option<String>, bool)> {
    events.iter().find_map(|e| match e {
        EngineEvent::Probed {
            total_bytes,
            etag,
            range_ok,
            ..
        } => Some((*total_bytes, etag.clone(), *range_ok)),
        _ => None,
    })
}

fn has_done(events: &[EngineEvent]) -> bool {
    events.iter().any(|e| matches!(e, EngineEvent::Done))
}

fn failed(events: &[EngineEvent]) -> Option<FailureReason> {
    events.iter().find_map(|e| match e {
        EngineEvent::Failed(r) => Some(r.clone()),
        _ => None,
    })
}

#[tokio::test]
async fn no_range_server_completes_single_connection() {
    let server = MockServer::start().await;
    let body = body_pattern(100_000);
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body.clone())
                .insert_header("etag", "W/\"abc\""),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("dl");
    let engine = ReqwestHttpEngine::for_tests();
    let (res, events) = run_collect(
        &engine,
        spec_for(server.uri(), dest.clone()),
        CancellationToken::new(),
        None,
    )
    .await;
    res.unwrap();

    let (total, etag, range_ok) = probed(&events).expect("Probed");
    assert_eq!(total, Some(100_000));
    assert_eq!(etag.as_deref(), Some("W/\"abc\""));
    assert!(!range_ok, "200 dönen sunucu range_ok olamaz");
    assert!(has_done(&events));
    assert_eq!(std::fs::read(dest.join("f.bin")).unwrap(), body);
    assert!(!dest.join("f.bin.part").exists(), ".part rename edilmeli");
}

#[tokio::test]
async fn chunked_without_length_is_unknown_total() {
    // wiremock Content-Length'i silemiyor; chunked'i kendimiz basıyoruz.
    let body = body_pattern(40_000);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let payload = body.clone();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { break };
            let payload = payload.clone();
            tokio::spawn(async move {
                let (r, mut w) = tokio::io::split(sock);
                let mut reader = tokio::io::BufReader::new(r);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 { return; }
                    if line == "\r\n" { break; }
                }
                let head = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
                if w.write_all(head).await.is_err() { return; }
                for chunk in payload.chunks(8192) {
                    let frame = format!("{:x}\r\n", chunk.len());
                    if w.write_all(frame.as_bytes()).await.is_err() { return; }
                    if w.write_all(chunk).await.is_err() { return; }
                    if w.write_all(b"\r\n").await.is_err() { return; }
                }
                let _ = w.write_all(b"0\r\n\r\n").await;
                let _ = w.flush().await;
            });
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("dl");
    let engine = ReqwestHttpEngine::for_tests();
    let (res, events) = run_collect(
        &engine,
        spec_for(format!("http://{addr}/f.bin"), dest.clone()),
        CancellationToken::new(),
        None,
    )
    .await;
    res.unwrap();

    let (total, _, range_ok) = probed(&events).expect("Probed");
    assert_eq!(total, None, "chunked + length yok → toplam bilinemez");
    assert!(!range_ok);
    assert!(has_done(&events));
    assert_eq!(std::fs::read(dest.join("f.bin")).unwrap(), body);
}

#[tokio::test]
async fn cancel_mid_download_keeps_part() {
    // Büyük gövde + yavaş sunucu yerine: indirmeyi hız limitiyle uzatıp
    // token'ı ortada kesiyoruz. Sunucu takılmıyor; cancel gerçekten kesiyor.
    let body = body_pattern(256 * 1024);
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("dl");
    let mut spec = spec_for(server.uri(), dest.clone());
    spec.speed_limit_bps = Some(32 * 1024); // 256KB @ 32KB/s ≈ 8s

    let engine = ReqwestHttpEngine::for_tests();
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let h = tokio::spawn(async move { engine.run(spec, tx, c2, None).await });

    let mut saw = 0u64;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Some(EngineEvent::Progress(p))) if p.downloaded_bytes > 8 * 1024 => {
                saw = p.downloaded_bytes;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }
    assert!(saw > 8 * 1024, "cancel öncesi ilerleme yok: {saw}");
    cancel.cancel();

    let res = tokio::time::timeout(Duration::from_secs(5), h)
        .await
        .expect("run cancel sonrası takıldı")
        .unwrap();
    res.unwrap();

    let part = dest.join("f.bin.part");
    assert!(part.exists(), ".part korunmalı");
    assert!(!dest.join("f.bin").exists(), "final oluşmamalı");
    let written = std::fs::read(&part).unwrap();
    assert!(!written.is_empty());
    assert_eq!(&written[..], &body[..written.len()], ".part prefix doğru olmalı");
}

#[tokio::test]
async fn task_speed_limit_paces() {
    let server = MockServer::start().await;
    let body = body_pattern(64 * 1024);
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let mut spec = spec_for(server.uri(), dir.path().join("dl"));
    spec.speed_limit_bps = Some(32 * 1024);

    let engine = ReqwestHttpEngine::for_tests();
    let start = tokio::time::Instant::now();
    let (res, events) = run_collect(&engine, spec, CancellationToken::new(), None).await;
    let elapsed = start.elapsed();
    res.unwrap();
    assert!(has_done(&events));
    assert!(
        elapsed >= Duration::from_millis(700),
        "görev limiti uygulanmadı: {elapsed:?}"
    );
    assert_eq!(std::fs::read(dir.path().join("dl/f.bin")).unwrap(), body);
}

#[tokio::test]
async fn global_limit_paces_when_task_unlimited() {
    let server = MockServer::start().await;
    let body = body_pattern(64 * 1024);
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let spec = spec_for(server.uri(), dir.path().join("dl"));
    let engine = ReqwestHttpEngine::for_tests();
    let global = Some(Arc::new(ByteBudget::new(32 * 1024)));
    let start = tokio::time::Instant::now();
    let (res, events) = run_collect(&engine, spec, CancellationToken::new(), global).await;
    res.unwrap();
    assert!(has_done(&events));
    assert!(
        start.elapsed() >= Duration::from_millis(700),
        "global limit uygulanmadı: {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn probe_500_retries_then_fails() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500).set_body_string("ise"))
        .expect(2..)
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = ReqwestHttpEngine::for_tests();
    let (res, events) = run_collect(
        &engine,
        spec_for(server.uri(), dir.path().join("dl")),
        CancellationToken::new(),
        None,
    )
    .await;
    res.unwrap();
    match failed(&events).expect("Failed") {
        FailureReason::Http { status, .. } => assert_eq!(status, 500),
        other => panic!("beklenen Http, gelen {other:?}"),
    }
    assert!(server.received_requests().await.unwrap().len() >= 2);
}

#[tokio::test]
async fn probe_403_is_not_reresolve() {
    // Probe 403: Range reddi olabilir. NeedReResolve yalnız indirme gövdesinde.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header("range", "bytes=0-0"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let body = body_pattern(200);
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("dl");
    let engine = ReqwestHttpEngine::for_tests();
    let (res, events) = run_collect(
        &engine,
        spec_for(server.uri(), dest.clone()),
        CancellationToken::new(),
        None,
    )
    .await;
    res.unwrap();
    assert!(
        !events.iter().any(|e| matches!(e, EngineEvent::NeedReResolve)),
        "probe 403 NeedReResolve olmamalı"
    );
    let (_, _, range_ok) = probed(&events).expect("Probed");
    assert!(!range_ok);
    assert!(has_done(&events));
    assert_eq!(std::fs::read(dest.join("f.bin")).unwrap(), body);
}

#[tokio::test]
async fn body_403_emits_need_reresolve() {
    // Probe 200 (range yok), gövde indirmesi 403 → URL öldü, kör retry yok.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header("range", "bytes=0-0"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8]))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = ReqwestHttpEngine::for_tests();
    let (res, events) = run_collect(
        &engine,
        spec_for(server.uri(), dir.path().join("dl")),
        CancellationToken::new(),
        None,
    )
    .await;
    res.unwrap();
    assert!(
        events.iter().any(|e| matches!(e, EngineEvent::NeedReResolve)),
        "indirme 403 → NeedReResolve"
    );
    let n = server.received_requests().await.unwrap().len();
    assert!(n <= 3, "403 kör retry edilmemeli, istek={n}");
}

#[tokio::test]
async fn custom_headers_reach_server() {
    let server = MockServer::start().await;
    let body = body_pattern(1024);
    Mock::given(method("GET"))
        .and(header("user-agent", "layer-test-ua"))
        .and(header("referer", "https://example.com/watch"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let mut spec = spec_for(server.uri(), dir.path().join("dl"));
    spec.headers = vec![
        ("User-Agent".into(), "layer-test-ua".into()),
        ("Referer".into(), "https://example.com/watch".into()),
    ];
    let engine = ReqwestHttpEngine::for_tests();
    let (res, events) = run_collect(&engine, spec, CancellationToken::new(), None).await;
    res.unwrap();
    assert!(has_done(&events));
    assert_eq!(std::fs::read(dir.path().join("dl/f.bin")).unwrap(), body);
}

#[tokio::test]
async fn probe_detects_range_support() {
    let server = MockServer::start().await;
    let body = body_pattern(10_000);
    Mock::given(method("GET"))
        .and(header("range", "bytes=0-0"))
        .respond_with(
            ResponseTemplate::new(206)
                .set_body_bytes(body[..1].to_vec())
                .insert_header("content-range", "bytes 0-0/10000")
                .insert_header("etag", "\"strong-1\""),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = ReqwestHttpEngine::for_tests();
    let (res, events) = run_collect(
        &engine,
        spec_for(server.uri(), dir.path().join("dl")),
        CancellationToken::new(),
        None,
    )
    .await;
    res.unwrap();
    let (total, etag, range_ok) = probed(&events).expect("Probed");
    assert!(range_ok);
    assert_eq!(total, Some(10_000));
    assert_eq!(etag.as_deref(), Some("\"strong-1\""));
    assert!(has_done(&events));
    assert_eq!(std::fs::read(dir.path().join("dl/f.bin")).unwrap(), body);
}

#[tokio::test]
async fn accept_ranges_lie_falls_back_to_single() {
    // Accept-Ranges: bytes diyor ama Range'e 200 (tam gövde) dönüyor.
    let server = MockServer::start().await;
    let body = body_pattern(8_000);
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body.clone())
                .insert_header("accept-ranges", "bytes"),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("dl");
    let engine = ReqwestHttpEngine::for_tests();
    let (res, events) = run_collect(
        &engine,
        spec_for(server.uri(), dest.clone()),
        CancellationToken::new(),
        None,
    )
    .await;
    res.unwrap();
    let (_, _, range_ok) = probed(&events).expect("Probed");
    assert!(!range_ok, "200 = Accept-Ranges yalan");
    // Tek segment planı resume içindir (core offset'i DB'ye yazar).
    // Çoklu SegmentPlan yalnız range_ok iken gelir — Task 9.
    let plans: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::SegmentPlan(s) => Some(s.len()),
            _ => None,
        })
        .collect();
    assert!(plans.iter().all(|n| *n <= 1), "yalancı range çoklu segment üretmemeli: {plans:?}");
    assert_eq!(std::fs::read(dest.join("f.bin")).unwrap(), body);
}

#[tokio::test]
async fn invalid_scheme_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let engine = ReqwestHttpEngine::for_tests();
    let (tx, _rx) = mpsc::unbounded_channel();
    let err = engine
        .run(
            spec_for("ftp://example.com/x".into(), dir.path().join("dl")),
            tx,
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, layer_engine_api::EngineError::InvalidSpec(_)));
}

#[tokio::test]
async fn existing_final_file_is_not_overwritten() {
    let server = MockServer::start().await;
    let body = body_pattern(100);
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("dl");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("f.bin"), b"existing!").unwrap();

    let engine = ReqwestHttpEngine::for_tests();
    let (res, events) = run_collect(
        &engine,
        spec_for(server.uri(), dest.clone()),
        CancellationToken::new(),
        None,
    )
    .await;
    res.unwrap();
    assert!(matches!(failed(&events), Some(FailureReason::Io(_))));
    assert_eq!(std::fs::read(dest.join("f.bin")).unwrap(), b"existing!");
}
