//! aria2 JSON-RPC. GID kalıcı kimlik değildir; görev kimliği bizim task id'miz.

use base64::Engine as _;
use layer_engine_api::{
    EngineError, EngineEvent, FailureReason, Progress, TorrentEngine, TorrentTaskSpec,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

struct Live {
    port: u16,
    secret: String,
    client: reqwest::Client,
    /// Proses yaşasın diye tutulur. Okunmaz; düşerse aria2 de düşer.
    #[allow(dead_code)]
    child: tokio::process::Child,
}

pub struct Aria2Engine {
    program: PathBuf,
    work: PathBuf,
    live: Mutex<Option<Live>>,
}

impl Aria2Engine {
    pub fn new(program: PathBuf, work: PathBuf) -> Self {
        Self {
            program,
            work,
            live: Mutex::new(None),
        }
    }

    async fn rpc(&self, method: &str, mut params: Vec<Value>) -> Result<Value, String> {
        let live = self.live.lock().await;
        let live = live.as_ref().ok_or("aria2 kapalı")?;
        params.insert(0, json!(format!("token:{}", live.secret)));
        let body = json!({ "jsonrpc": "2.0", "id": "layer", "method": method, "params": params });
        let url = format!("http://127.0.0.1:{}/jsonrpc", live.port);
        let resp = live
            .client
            .post(url)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let text = resp.text().await.map_err(|e| e.to_string())?;
        let value: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if let Some(err) = value.get("error") {
            return Err(err.to_string());
        }
        Ok(value["result"].clone())
    }

    async fn ensure(&self) -> Result<(), String> {
        {
            let guard = self.live.lock().await;
            if guard.is_some() {
                return Ok(());
            }
        }
        std::fs::create_dir_all(&self.work).map_err(|e| e.to_string())?;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        drop(listener);
        let secret = uuid_like();
        let session = self.work.join("aria2.session");
        let mut cmd = Command::new(&self.program);
        cmd.arg("--enable-rpc")
            .arg("--rpc-listen-all=false")
            .arg(format!("--rpc-listen-port={port}"))
            .arg(format!("--rpc-secret={secret}"))
            .arg("--seed-time=0")
            .arg("--seed-ratio=0.0")
            .arg("--enable-dht=true")
            .arg("--bt-enable-lpd=true")
            .arg("--enable-peer-exchange=true")
            .arg(format!("--save-session={}", session.display()))
            .arg("--save-session-interval=30")
            .arg("--console-log-level=warn")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if session.is_file() {
            cmd.arg(format!("--input-file={}", session.display()));
        }
        let child = cmd.spawn().map_err(|e| e.to_string())?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| e.to_string())?;
        {
            let mut guard = self.live.lock().await;
            *guard = Some(Live {
                port,
                secret,
                client,
                child,
            });
        }
        // RPC ayağa kalkana kadar kısa bekleme.
        for _ in 0..20 {
            if self.rpc("aria2.getVersion", vec![]).await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err("aria2 RPC açılmadı".into())
    }
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(1);
    format!("{n:x}")
}

fn is_magnet(source: &str) -> bool {
    source.trim().starts_with("magnet:")
}

#[async_trait::async_trait]
impl TorrentEngine for Aria2Engine {
    async fn run(
        &self,
        spec: TorrentTaskSpec,
        events: tokio::sync::mpsc::UnboundedSender<EngineEvent>,
        cancel: CancellationToken,
    ) -> Result<(), EngineError> {
        std::fs::create_dir_all(&spec.dest_dir)?;
        self.ensure()
            .await
            .map_err(|e| EngineError::Internal(e))?;
        let dir = spec.dest_dir.to_string_lossy().to_string();
        let options = json!({ "dir": dir, "seed-time": "0", "seed-ratio": "0.0" });
        let gid = if is_magnet(&spec.source) {
            self.rpc(
                "aria2.addUri",
                vec![json!([spec.source]), options],
            )
            .await
        } else {
            let bytes = std::fs::read(&spec.source).map_err(|e| EngineError::Io(e))?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
            self.rpc(
                "aria2.addTorrent",
                vec![json!(encoded), json!([]), options],
            )
            .await
        };
        let gid = match gid {
            Ok(v) => v.as_str().unwrap_or("").to_string(),
            Err(e) => {
                let _ = events.send(EngineEvent::Failed(FailureReason::Torrent(e)));
                return Ok(());
            }
        };
        if gid.is_empty() {
            let _ = events.send(EngineEvent::Failed(FailureReason::Torrent(
                "aria2 gid vermedi".into(),
            )));
            return Ok(());
        }

        loop {
            if cancel.is_cancelled() {
                let _ = self.rpc("aria2.forcePause", vec![json!(gid)]).await;
                return Ok(());
            }
            let status = self
                .rpc(
                    "aria2.tellStatus",
                    vec![json!(gid), json!(["status", "totalLength", "completedLength", "downloadSpeed", "bittorrent", "errorMessage"])],
                )
                .await;
            let status = match status {
                Ok(v) => v,
                Err(e) => {
                    let _ = events.send(EngineEvent::Failed(FailureReason::Torrent(e)));
                    return Ok(());
                }
            };
            let downloaded = status["completedLength"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let total = status["totalLength"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|n| *n > 0);
            let speed = status["downloadSpeed"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            if let Some(name) = status
                .pointer("/bittorrent/info/name")
                .and_then(|v| v.as_str())
            {
                let _ = events.send(EngineEvent::Named {
                    file_name: name.to_string(),
                });
            }
            let _ = events.send(EngineEvent::Progress(Progress {
                downloaded_bytes: downloaded,
                total_bytes: total,
                speed_bps: speed,
            }));
            match status["status"].as_str() {
                Some("complete") => {
                    let _ = events.send(EngineEvent::Done);
                    return Ok(());
                }
                Some("error") => {
                    let msg = status["errorMessage"]
                        .as_str()
                        .unwrap_or("aria2 hata")
                        .to_string();
                    let _ = events.send(EngineEvent::Failed(FailureReason::Torrent(msg)));
                    return Ok(());
                }
                Some("removed") => {
                    let _ = events.send(EngineEvent::Failed(FailureReason::Torrent(
                        "aria2 görevi kaldırdı".into(),
                    )));
                    return Ok(());
                }
                _ => {}
            }
            tokio::select! {
                _ = cancel.cancelled() => {
                    let _ = self.rpc("aria2.forcePause", vec![json!(gid)]).await;
                    return Ok(());
                }
                _ = tokio::time::sleep(Duration::from_millis(400)) => {}
            }
        }
    }
}
