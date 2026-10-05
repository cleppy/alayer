//! aria2 JSON-RPC. GID kalıcı kimlik değildir; görev kimliği bizim task id'miz.

use base64::Engine as _;
use layer_engine_api::{
    EngineError, EngineEvent, FailureReason, Progress, TorrentEngine, TorrentFile, TorrentTaskSpec,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex as StdMutex;
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
    picks: StdMutex<HashMap<String, tokio::sync::oneshot::Sender<Vec<u32>>>>,
    gids: StdMutex<HashMap<String, String>>,
    global_limit: StdMutex<Option<u64>>,
}

impl Aria2Engine {
    pub fn new(program: PathBuf, work: PathBuf) -> Self {
        Self {
            program,
            work,
            live: Mutex::new(None),
            picks: StdMutex::new(HashMap::new()),
            gids: StdMutex::new(HashMap::new()),
            global_limit: StdMutex::new(None),
        }
    }

    pub fn submit_files(&self, task_id: &str, indices: Vec<u32>) -> bool {
        if let Some(tx) = self
            .picks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(task_id)
        {
            tx.send(indices).is_ok()
        } else {
            false
        }
    }

    /// aria2 kapalıysa yalnız hatırlar. Açıkken tavan hemen değişir.
    pub async fn set_global_limit(&self, bps: Option<u64>) {
        let bps = bps.filter(|n| *n > 0);
        *self.global_limit.lock().unwrap_or_else(|e| e.into_inner()) = bps;
        if self.live.lock().await.is_none() {
            return;
        }
        self.apply_global_limit().await;
    }

    pub async fn set_task_limit(&self, task_id: &str, bps: Option<u64>) {
        let gid = self
            .gids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(task_id)
            .cloned();
        let Some(gid) = gid else { return };
        if self.live.lock().await.is_none() {
            return;
        }
        let value = bps.unwrap_or(0).to_string();
        let _ = self
            .rpc(
                "aria2.changeOption",
                vec![json!(gid), json!({ "max-download-limit": value })],
            )
            .await;
    }

    pub async fn save_session(&self) {
        if self.live.lock().await.is_none() {
            return;
        }
        let _ = self.rpc("aria2.saveSession", vec![]).await;
    }

    async fn apply_global_limit(&self) {
        let value = self
            .global_limit
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .unwrap_or(0)
            .to_string();
        let _ = self
            .rpc(
                "aria2.changeGlobalOption",
                vec![json!({ "max-overall-download-limit": value })],
            )
            .await;
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
                let _ = self.rpc("aria2.forcePauseAll", vec![]).await;
                self.apply_global_limit().await;
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

impl Aria2Engine {
    async fn tell(&self, gid: &str) -> Option<String> {
        let status = self
            .rpc("aria2.tellStatus", vec![json!(gid), json!(["status"])])
            .await
            .ok()?;
        status["status"].as_str().map(str::to_string)
    }

    async fn add_download(&self, spec: &TorrentTaskSpec, gid: &str) -> Result<(), String> {
        let dir = spec.dest_dir.to_string_lossy().to_string();
        let mut options = json!({
            "dir": dir,
            "seed-time": "0",
            "seed-ratio": "0.0",
            "pause": "true",
            "gid": gid,
        });
        if let Some(bps) = spec.speed_limit_bps.filter(|n| *n > 0) {
            options["max-download-limit"] = json!(bps.to_string());
        }
        if let Some(indices) = spec.selected_files.as_ref().filter(|v| !v.is_empty()) {
            options["select-file"] = json!(indices
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(","));
        }
        let added = if is_magnet(&spec.source) {
            self.rpc("aria2.addUri", vec![json!([spec.source.clone()]), options])
                .await
        } else {
            let bytes = std::fs::read(&spec.source).map_err(|e| e.to_string())?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
            self.rpc("aria2.addTorrent", vec![json!(encoded), json!([]), options])
                .await
        };
        match added {
            Ok(_) => Ok(()),
            Err(e) if e.to_lowercase().contains("gid") => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn select_files(&self, gid: &str, indices: &[u32]) {
        let list = indices
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let _ = self
            .rpc(
                "aria2.changeOption",
                vec![json!(gid), json!({ "select-file": list })],
            )
            .await;
    }

    async fn wait_files(
        &self,
        gid: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<TorrentFile>, String> {
        for _ in 0..360 {
            if cancel.is_cancelled() {
                return Ok(vec![]);
            }
            let status = self
                .rpc(
                    "aria2.tellStatus",
                    vec![json!(gid), json!(["files", "status", "errorMessage"])],
                )
                .await?;
            if status["status"].as_str() == Some("error") {
                return Err(status["errorMessage"]
                    .as_str()
                    .unwrap_or("aria2 error")
                    .to_string());
            }
            let files = files_of(&status);
            if !files.is_empty() {
                return Ok(files);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Err("torrent metadata timed out".into())
    }
}

fn files_of(status: &Value) -> Vec<TorrentFile> {
    let Some(list) = status["files"].as_array() else {
        return vec![];
    };
    list.iter()
        .filter_map(|file| {
            let index = file["index"].as_str()?.parse().ok()?;
            let length = file["length"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let path = file["path"].as_str().unwrap_or("file");
            let name = std::path::Path::new(path)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("file {index}"));
            Some(TorrentFile {
                index,
                name,
                length,
            })
        })
        .collect()
}

fn stable_gid(task_id: &str) -> String {
    let hex: String = task_id
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let mut gid = if hex.len() >= 16 {
        hex[..16].to_string()
    } else {
        format!("{hex:0<16}")
    };
    if gid.chars().all(|c| c == '0') {
        gid = "0000000000000001".into();
    }
    gid
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
        self.ensure().await.map_err(|e| EngineError::Internal(e))?;
        let gid = stable_gid(&spec.task_id);
        self.gids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(spec.task_id.clone(), gid.clone());
        if let Some(status) = self.tell(&gid).await {
            match status.as_str() {
                "complete" => {
                    let _ = events.send(EngineEvent::Done);
                    return Ok(());
                }
                "error" | "removed" => {
                    let _ = self.rpc("aria2.forceRemove", vec![json!(gid)]).await;
                    if let Err(e) = self.add_download(&spec, &gid).await {
                        let _ = events.send(EngineEvent::Failed(FailureReason::Torrent(e)));
                        return Ok(());
                    }
                }
                _ => {}
            }
        } else if let Err(e) = self.add_download(&spec, &gid).await {
            let _ = events.send(EngineEvent::Failed(FailureReason::Torrent(e)));
            return Ok(());
        }
        if cancel.is_cancelled() {
            let _ = self.rpc("aria2.forcePause", vec![json!(gid)]).await;
            return Ok(());
        }

        if let Some(indices) = spec.selected_files.filter(|v| !v.is_empty()) {
            let _ = self.rpc("aria2.forcePause", vec![json!(gid)]).await;
            self.select_files(&gid, &indices).await;
        } else {
            let _ = self.rpc("aria2.forcePause", vec![json!(gid)]).await;
            let files = self.wait_files(&gid, &cancel).await;
            if cancel.is_cancelled() {
                let _ = self.rpc("aria2.forcePause", vec![json!(gid)]).await;
                return Ok(());
            }
            let files = match files {
                Ok(files) if !files.is_empty() => files,
                Ok(_) => {
                    let _ = events.send(EngineEvent::Failed(FailureReason::Torrent(
                        "torrent has no files".into(),
                    )));
                    return Ok(());
                }
                Err(e) => {
                    let _ = events.send(EngineEvent::Failed(FailureReason::Torrent(e)));
                    return Ok(());
                }
            };
            if files.len() > 1 {
                let (tx, rx) = tokio::sync::oneshot::channel();
                self.picks
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(spec.task_id.clone(), tx);
                let _ = events.send(EngineEvent::TorrentFiles { files });
                let chosen = tokio::select! {
                    _ = cancel.cancelled() => {
                        self.picks.lock().unwrap_or_else(|e| e.into_inner()).remove(&spec.task_id);
                        None
                    }
                    pick = rx => pick.ok().filter(|v| !v.is_empty()),
                };
                let Some(indices) = chosen else {
                    if cancel.is_cancelled() {
                        let _ = self.rpc("aria2.forcePause", vec![json!(gid)]).await;
                    } else {
                        let _ = self.rpc("aria2.forceRemove", vec![json!(gid)]).await;
                        let _ = events.send(EngineEvent::Failed(FailureReason::Torrent(
                            "no files selected".into(),
                        )));
                    }
                    return Ok(());
                };
                self.select_files(&gid, &indices).await;
            }
        }
        let _ = self.rpc("aria2.unpause", vec![json!(gid)]).await;

        loop {
            if cancel.is_cancelled() {
                let _ = self.rpc("aria2.forcePause", vec![json!(gid)]).await;
                return Ok(());
            }
            let status = self
                .rpc(
                    "aria2.tellStatus",
                    vec![
                        json!(gid),
                        json!([
                            "status",
                            "totalLength",
                            "completedLength",
                            "downloadSpeed",
                            "bittorrent",
                            "errorMessage"
                        ]),
                    ],
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
