# layer M0+M1: Workspace + core + engine-http Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Headless çalışan indirme çekirdeği: `engine-api` (portlar/DTO'lar), `core` (durum makinesi + SQLite + kuyruk + TaskManager + TaskIntake), `engine-http` (probe, segmented indirme, resume, If-Range conflict, retry, 403→NeedReResolve, hız limiti) ve uçtan uca doğrulayan minimal CLI.

**Architecture:** Dependency Rule — `core` yalnız `engine-api`'yi tanır; `engine-http` yalnız `engine-api`'yi tanır; composition root (CLI, ileride src-tauri) ikisini birleştirir. SQLite tek doğruluk kaynağı; `.part` sadece byte'lar. Engine'ler `run(spec, events_tx, cancel)` sözleşmesiyle çalışır.

**Tech Stack:** Rust stable (MSVC), tokio, reqwest (rustls), rusqlite (bundled), serde, uuid, thiserror, async-trait, tokio-util (CancellationToken), wiremock (test), tempfile (test).

**Spec:** `docs/superpowers/specs/2026-10-05-layer-design.md`

## Global Constraints

- Rust edition 2021, stable MSVC toolchain (`rust-toolchain.toml` ile pinli)
- Crate adları: `layer-engine-api`, `layer-core`, `layer-engine-http`, `layer-cli` (dizinler: `crates/engine-api`, `crates/core`, `crates/engine-http`, `apps/cli`)
- `core` crate'inde `reqwest`, `tokio::process`, Tauri, aria2, yt-dlp isimleri GEÇMEZ (tokio sync/time primitifleri serbest)
- `engine-http` crate'inde `layer-core` bağımlılığı OLAMAZ
- TaskId = uuid v4 string; kalıcı kimlik. GID (M3) sadece cache
- DB dosyası: testlerde tempdir, üretimde app data dizini (M2'de bağlanır; CLI `--db` ile alır)
- Hız event'leri throttle edilmez (engine ~1Hz progress üretir; UI throttle'ı M2 kabuğunun işi)
- Her task sonunda `cargo test -p <crate>` yeşil + `cargo clippy --workspace -- -D warnings` temiz → commit
- Commit mesajları conventional: `feat:`, `test:`, `chore:`, `fix:`

## Not: Kapsam ertelemeleri

- Sidecar pin+checksum aracı (spec M0) → ilk sidecar kullanan milestone'a (M3) ertelendi; M1'de sidecar yok.
- `TorrentEngine`, `MediaResolver`, `ClipboardWatcher` portları → kendi milestone'larında (M3/M4/M5) `engine-api`'ye eklenir. M1'de `TaskKind::Torrent/Media` intake'te kabul edilir ama dispatch edilmez (`Queued` bekler, log: engine yok).

---

### Task 1: Workspace iskeleti (M0)

**Files:**
- Create: `Cargo.toml` (workspace root), `rust-toolchain.toml`, `.gitignore`, `README.md`
- Create: `crates/engine-api/Cargo.toml`, `crates/engine-api/src/lib.rs`
- Create: `crates/core/Cargo.toml`, `crates/core/src/lib.rs`
- Create: `crates/engine-http/Cargo.toml`, `crates/engine-http/src/lib.rs`
- Create: `apps/cli/Cargo.toml`, `apps/cli/src/main.rs`

**Interfaces:**
- Consumes: —
- Produces: `cargo check --workspace` geçen iskelet; sonraki tüm tasklar bu dizin yapısını kullanır.

- [ ] **Step 1: Workspace dosyalarını yaz**

Root `Cargo.toml`:
```toml
[workspace]
resolver = "2"
members = ["crates/engine-api", "crates/core", "crates/engine-http", "apps/cli"]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
uuid = { version = "1", features = ["v4", "serde"] }
tokio = { version = "1", features = ["full"] }
tokio-util = "0.7"
async-trait = "0.1"
tracing = "0.1"
rusqlite = { version = "0.32", features = ["bundled"] }
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "stream", "gzip", "deflate"] }
futures = "0.3"
tempfile = "3"
wiremock = "0.6"
```

`rust-toolchain.toml`:
```toml
[toolchain]
channel = "stable"
components = ["clippy", "rustfmt"]
```

`.gitignore`: `target/`, `node_modules/`, `dist/`, `*.part`, `*.aria2`, `layer.db*`

Crate Cargo.toml'ları (engine-api örneği — core ve engine-http benzer, bağımlılıklar task'larda):
```toml
[package]
name = "layer-engine-api"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
serde = { workspace = true }
thiserror = { workspace = true }
uuid = { workspace = true }
tokio = { workspace = true }
tokio-util = { workspace = true }
async-trait = { workspace = true }
```

`apps/cli/src/main.rs` başlangıç stub: `fn main() { println!("layer-cli"); }`

- [ ] **Step 2: Doğrula** — `cargo check --workspace` → PASS (üç boş lib + cli derlenir)
- [ ] **Step 3: Commit** — `chore: workspace scaffold (engine-api, core, engine-http, cli)`

---

### Task 2: engine-api — DTO'lar

**Files:**
- Modify: `crates/engine-api/src/lib.rs` (module tree: `types.rs`)
- Create: `crates/engine-api/src/types.rs`
- Test: `crates/engine-api/src/types.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Produces (Task 4,5,6,7,8,9,10'un tüketeceği tam tipler):
```rust
pub type TaskId = String; // uuid v4

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind { Http, Torrent, Media }

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "reason", content = "detail", rename_all = "snake_case")]
pub enum FailureReason {
    Network(String),
    Http { status: u16, message: String },
    ResolverRequired,          // 403/410 ve resolver yok/yeniden çözüm başarısız
    Resolver(String),          // yt-dlp hata sınıflandırması (M4)
    Io(String),
    Torrent(String),           // aria2 hata sınıflandırması (M3)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState { Queued, Downloading, Paused, Completed, Failed, Conflict }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Progress {
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub speed_bps: f64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SegmentProgress { pub idx: u32, pub start: u64, pub end: u64, pub downloaded: u64 }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResumeState {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub total_bytes: Option<u64>,
    pub segments: Vec<SegmentProgress>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HttpTaskSpec {
    pub task_id: TaskId,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub dest_dir: std::path::PathBuf,
    pub file_name: String,
    pub max_segments: usize,
    pub speed_limit_bps: Option<u64>,
    pub resume: Option<ResumeState>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResolverContext {
    pub original_url: String,
    pub extractor: String,
    pub format_id: String,
    pub expected_size: Option<u64>,
}

/// Engine → core olay akışı (run() başına bir mpsc kanalı)
#[derive(Debug, Clone)]
pub enum EngineEvent {
    Probed { total_bytes: Option<u64>, etag: Option<String>, last_modified: Option<String>, range_ok: bool },
    Progress(Progress),
    SegmentPlan(Vec<SegmentProgress>),          // DB'ye segment durumunu yazmak için
    SegmentUpdate { idx: u32, downloaded: u64 }, // incremental segment ilerlemesi (periyodik flush)
    Done,                                        // final dosya yerine taşındı
    Failed(FailureReason),
    ConflictDetected,                            // If-Range 200 döndü / kaynak değişti
    NeedReResolve,                               // 403/410 — ResolverContext ile tazeleme gerekli
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("invalid spec: {0}")] InvalidSpec(String),
    #[error("io: {0}")] Io(#[from] std::io::Error),
    #[error("internal: {0}")] Internal(String),
}
```

- [ ] **Step 1: Testleri yaz (serde roundtrip + TaskState serde isimleri)**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn task_kind_serde_snake_case() {
        assert_eq!(serde_json::to_string(&TaskKind::Http).unwrap(), "\"http\"");
    }
    #[test] fn failure_reason_roundtrip() {
        let r = FailureReason::Http { status: 403, message: "gone".into() };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"reason\":\"http\""));
        assert_eq!(serde_json::from_str::<FailureReason>(&j).unwrap(), r);
    }
    #[test] fn resume_state_roundtrip() {
        let rs = ResumeState { etag: Some("W/\"abc\"".into()), last_modified: None,
            total_bytes: Some(100), segments: vec![SegmentProgress { idx: 0, start: 0, end: 99, downloaded: 40 }] };
        let j = serde_json::to_string(&rs).unwrap();
        let back: ResumeState = serde_json::from_str(&j).unwrap();
        assert_eq!(back.segments[0].downloaded, 40);
    }
}
```
- [ ] **Step 2: `cargo test -p layer-engine-api` → FAIL (tipler yok)**
- [ ] **Step 3: types.rs'i yukarıdaki tanımlarla yaz, `lib.rs`'te `pub mod types; pub use types::*;`**
- [ ] **Step 4: `cargo test -p layer-engine-api` → PASS; `cargo clippy -p layer-engine-api -- -D warnings` temiz**
- [ ] **Step 5: Commit** — `feat(engine-api): core DTO types`

---

### Task 3: engine-api — portlar + ByteBudget

**Files:**
- Create: `crates/engine-api/src/ports.rs`, `crates/engine-api/src/budget.rs`
- Modify: `crates/engine-api/src/lib.rs`
- Test: `crates/engine-api/src/budget.rs` (inline)

**Interfaces:**
- Produces:
```rust
// ports.rs
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Dış dünya (UI/kabuk) core'dan olay alır.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: CoreEvent);
}

/// Core → dış dünya olayları (Task 7'de tam liste; UI bunları Tauri event'ine çevirir).
#[derive(Debug, Clone)]
pub enum CoreEvent {
    TaskAdded { task_id: TaskId, kind: TaskKind },
    TaskStateChanged { task_id: TaskId, old: TaskState, new: TaskState, failure: Option<FailureReason> },
    Progress { task_id: TaskId, progress: Progress },
    Conflict { task_id: TaskId },
    IntakePrompt { task_id: TaskId, url: String, source: IntakeSource }, // clipboard "sor" modu (M5'te kullanılır)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntakeSource { Ui, Clipboard, Cli, Extension }

#[async_trait::async_trait]
pub trait HttpEngine: Send + Sync {
    /// Görevi tamamlanana/karışıklık/çözüm-yenileme/iptal durumuna kadar sürer.
    /// pause = cancel.cancel() (core yorumlar), iptal = aynı mekanizma + DB silme.
    async fn run(
        &self,
        spec: HttpTaskSpec,
        events: mpsc::UnboundedSender<EngineEvent>,
        cancel: CancellationToken,
    ) -> Result<(), EngineError>;
}

// budget.rs — paylaşılan byte-budget token bucket (per-task ve global)
#[derive(Debug)]
pub struct ByteBudget { /* capacity_bps: u64, tokens: Mutex<f64>, last: Mutex<Instant> */ }
impl ByteBudget {
    pub fn new(capacity_bps: u64) -> Self;
    /// n byte hakkı ister; gerekirse uyur. cancel ile kesilebilir.
    pub async fn acquire(&self, n: u64, cancel: &CancellationToken);
}
```

- [ ] **Step 1: ByteBudget testini yaz**
```rust
#[tokio::test]
async fn budget_paces_bytes() {
    let b = ByteBudget::new(10_000); // 10 KB/s
    let cancel = CancellationToken::new();
    let start = tokio::time::Instant::now();
    b.acquire(10_000, &cancel).await; // mevcut kredi
    b.acquire(5_000, &cancel).await;  // ~0.5s beklemeli
    assert!(start.elapsed() >= std::time::Duration::from_millis(400));
}
#[tokio::test]
async fn budget_acquire_cancellable() {
    let b = ByteBudget::new(1);
    let cancel = CancellationToken::new();
    cancel.cancel();
    b.acquire(1_000_000, &cancel).await; // hemen dönmeli (panik yok)
}
```
- [ ] **Step 2: `cargo test -p layer-engine-api` → FAIL**
- [ ] **Step 3: ports.rs + budget.rs implement (ByteBudget: `tokio::sync::Mutex`, refill = elapsed*capacity; acquire: kredi yetmiyorsa `tokio::select! { sleep, cancelled }`)**
- [ ] **Step 4: `cargo test -p layer-engine-api` → PASS; clippy temiz**
- [ ] **Step 5: Commit** — `feat(engine-api): ports (HttpEngine, EventSink) + ByteBudget`

---

### Task 4: core — Task entity + durum makinesi

**Files:**
- Create: `crates/core/src/task.rs`
- Modify: `crates/core/src/lib.rs` (`pub mod task;`)
- Modify: `crates/core/Cargo.toml` (deps: `layer-engine-api = { path = "../engine-api" }`, serde, serde_json, uuid, thiserror, tracing, tokio (sync/time/rt), rusqlite)
- Test: `crates/core/src/task.rs` (inline)

**Interfaces:**
- Produces:
```rust
pub struct Task {
    pub id: TaskId,
    pub kind: TaskKind,
    pub state: TaskState,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub dest_dir: PathBuf,
    pub file_name: String,
    pub total_bytes: Option<u64>,
    pub downloaded_bytes: u64,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub range_ok: bool,
    pub unresumable: bool,
    pub resolver_ctx: Option<ResolverContext>,
    pub failure: Option<FailureReason>,
    pub speed_limit_bps: Option<u64>,
    pub segments: Vec<SegmentProgress>,
    pub created_at: i64, // unix ms
    pub updated_at: i64,
}
impl Task {
    pub fn new_http(url: String, dest_dir: PathBuf, file_name: Option<String>) -> Self; // id=uuid, state=Queued, file_name yoksa URL'den türet
    /// İzinli geçişler: Queued→{Downloading,Paused}, Downloading→{Paused,Completed,Failed,Conflict},
    /// Paused→Queued, Failed→Queued (retry), Conflict→Queued (restart_from_scratch=true ile segments sıfırlanır)
    pub fn transition(&mut self, to: TaskState) -> Result<(), InvalidTransition>;
    pub fn is_active(&self) -> bool; // Downloading
    pub fn is_finished(&self) -> bool; // Completed
}
#[derive(Debug, thiserror::Error)]
#[error("illegal transition {from:?} -> {to:?}")]
pub struct InvalidTransition { pub from: TaskState, pub to: TaskState }
```

- [ ] **Step 1: Testleri yaz** — tüm izinli geçişler OK; `Completed→Downloading`, `Queued→Completed` gibi izinliler `InvalidTransition`; `Conflict→Queued` segmentleri sıfırlar; `new_http` file_name türetme (`https://x.com/a/b.zip?q=1` → `b.zip`, boş path → `download`).
- [ ] **Step 2: FAIL → 3. implement → 4. PASS + clippy → 5. Commit** — `feat(core): Task entity + state machine`

---

### Task 5: core — SQLite repository

**Files:**
- Create: `crates/core/src/db.rs`
- Modify: `crates/core/src/lib.rs`
- Test: `crates/core/src/db.rs` (inline, `tempfile`)

**Interfaces:**
- Produces:
```rust
pub struct Db { conn: rusqlite::Connection /* M1: tek bağlantı + Mutex; TaskManager Arc<Db> paylaşır */ }
impl Db {
    pub fn open(path: &std::path::Path) -> Result<Self, DbError>; // ":memory:" destekli
    pub fn upsert_task(&self, t: &Task) -> Result<(), DbError>;   // segments+resolver_ctx JSON kolonlara
    pub fn load_task(&self, id: &TaskId) -> Result<Option<Task>, DbError>;
    pub fn list_tasks(&self) -> Result<Vec<Task>, DbError>;       // created_at ASC
    pub fn delete_task(&self, id: &TaskId) -> Result<(), DbError>;
    // settings
    pub fn get_setting(&self, key: &str) -> Result<Option<String>, DbError>;
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), DbError>;
}
```
Şema: `tasks(id TEXT PK, kind TEXT, state TEXT, url TEXT, headers TEXT, dest_dir TEXT, file_name TEXT, total_bytes INT, downloaded_bytes INT NOT NULL DEFAULT 0, etag TEXT, last_modified TEXT, range_ok INT NOT NULL DEFAULT 0, unresumable INT NOT NULL DEFAULT 0, resolver_ctx TEXT, failure TEXT, speed_limit_bps INT, segments TEXT NOT NULL DEFAULT '[]', created_at INT NOT NULL, updated_at INT NOT NULL)`; `settings(key TEXT PK, value TEXT NOT NULL)`. Migration: `user_version` pragma ile basit versiyonlama (v1).

- [ ] **Step 1: Testleri yaz** — `:memory:` upsert→load roundtrip (tüm alanlar, segments JSON, resolver_ctx); state güncelleme; delete; list sırası; settings get/set; olmayan task → `Ok(None)`.
- [ ] **Step 2: FAIL → 3. implement → 4. PASS + clippy → 5. Commit** — `feat(core): SQLite task repository`

---

### Task 6: core — kuyruk politikası

**Files:**
- Create: `crates/core/src/queue.rs`
- Modify: `crates/core/src/lib.rs`
- Test: inline

**Interfaces:**
- Produces:
```rust
pub struct QueuePolicy { pub max_concurrent: usize } // default 3
impl QueuePolicy {
    /// Aktif görev sayısı < max_concurrent ise beklemedeki en eski görevi döner.
    pub fn next(&self, tasks: &[Task]) -> Option<TaskId>; // state==Queued, created_at en küçük
}
```
(Not: hız limiti bütçesi ByteBudget engine-api'de; core TaskManager'da global budget'ı kurar ve spec'e koyar — Task 7.)

- [ ] **Step 1: Testler** — max_concurrent doluyken `None`; yer açılınca en eski Queued; aktif olmayan state'ler (Paused/Failed/Completed) sayılmaz.
- [ ] **Step 2: FAIL → 3. implement → 4. PASS → 5. Commit** — `feat(core): queue policy`

---

### Task 7: core — TaskManager + TaskIntake (use-case katmanı)

**Files:**
- Create: `crates/core/src/manager.rs`, `crates/core/src/intake.rs`, `crates/core/src/error.rs`
- Modify: `crates/core/src/lib.rs`
- Test: `crates/core/src/manager.rs` (inline; `FakeEngine`)

**Interfaces:**
- Consumes: `HttpEngine`, `EventSink`, `CoreEvent`, `EngineEvent`, `HttpTaskSpec`, `ByteBudget` (engine-api); `Task`, `Db`, `QueuePolicy` (Task 4-6)
- Produces:
```rust
pub struct TaskManager { /* Arc<Db>, Arc<dyn HttpEngine>, Arc<dyn EventSink>, QueuePolicy,
                            Arc<ByteBudget> (global, Option), Mutex<HashMap<TaskId, CancellationToken>>,
                            JoinSet<()> veya spawn'lı run döngüsü, Notify (kuyruk uyandırma) */ }
impl TaskManager {
    pub fn new(db: Arc<Db>, http: Arc<dyn HttpEngine>, sink: Arc<dyn EventSink>,
               policy: QueuePolicy, global_limit_bps: Option<u64>) -> Self;
    /// Açılışta: DB'deki Downloading görevleri Paused'a çek (çökme kurtarması), pump'u başlat.
    pub async fn start(&self) -> Result<(), CoreError>;
    pub async fn intake(&self, source: IntakeSource, url: String, dest_dir: PathBuf) -> Result<TaskId, CoreError>; // TaskIntake kapısı — kind URL şemasından: magnet:?xt → Torrent, http(s) → Http
    pub async fn pause(&self, id: &TaskId) -> Result<(), CoreError>;   // cancel token; state Paused
    pub async fn resume(&self, id: &TaskId) -> Result<(), CoreError>;  // Paused|Failed|Conflict → Queued + resume state DB'den
    pub async fn cancel(&self, id: &TaskId) -> Result<(), CoreError>;  // token + DB delete + .part sil
    pub async fn retry(&self, id: &TaskId) -> Result<(), CoreError>;   // = resume alias (Failed→Queued)
    pub async fn set_task_speed_limit(&self, id: &TaskId, bps: Option<u64>) -> Result<(), CoreError>;
    pub async fn shutdown(&self); // tüm token'lar cancel, pump durur
}
#[derive(Debug, thiserror::Error)] pub enum CoreError { NotFound, InvalidState(String), Db(String), Engine(String) }
```
Davranış kuralları:
- `intake`: `Task::new_http` (magnet → `TaskKind::Torrent`, M1'de dispatch edilmez, Queued kalır) → `Db::upsert_task` → `CoreEvent::TaskAdded` → pump uyandır.
- pump: `QueuePolicy::next` ile görev seçer → `Downloading`'e geçirir → `HttpTaskSpec` kurar (headers, max_segments=settings'ten, speed_limit, resume=Some ise DB'deki segments+etag) → `HttpEngine::run`'ı spawn eder, events kanalını task döngüsüne bağlar.
- event işleme: `Probed` → total/etag/last_modified/range_ok/unresumable=!range_ok DB; `SegmentPlan`/`SegmentUpdate` → segments DB (periyodik flush, ~1s); `Progress` → downloaded_bytes DB + `CoreEvent::Progress`; `Done` → `Completed` + `TaskStateChanged`; `Failed(r)` → state Failed + failure DB + event; `ConflictDetected` → `Conflict` + `CoreEvent::Conflict`; `NeedReResolve` → M1'de `Failed(ResolverRequired)`.
- `pause`: token cancel → run biter → engine'in bıraktığı downloaded DB'de zaten → state Paused. `resume`: Queued + resume state spec'e.
- Çökme kurtarması (`start`): state==Downloading olan her görev → Paused (segments DB'de; resume hazır).

**FakeEngine (test):** senaryo script'li — `Vec<EngineEvent>` yayınlar, `cancel` bekler, çağrıları kaydeder (`Mutex<Vec<HttpTaskSpec>>`).

- [ ] **Step 1: Testleri yaz**
  - intake → DB'de Queued görev + TaskAdded eventi
  - start → FakeEngine'e spec gelir, Probed/Progress/Done → state Completed, total/etag DB'de
  - pause → token cancel, state Paused; resume → yeni run, spec.resume dolu (segments korunmuş)
  - ConflictDetected → state Conflict + CoreEvent::Conflict
  - NeedReResolve → Failed(ResolverRequired)
  - max_concurrent=1: iki intake → sadece biri Downloading, diğeri Completed sonrası başlar
  - start() çökme kurtarması: DB'ye elle Downloading yazılmış görev → start sonrası Paused
  - magnet intake → Queued kalır, engine çağrılmaz
- [ ] **Step 2: `cargo test -p layer-core` → FAIL**
- [ ] **Step 3: manager.rs + intake.rs + error.rs implement**
- [ ] **Step 4: PASS + clippy → 5. Commit** — `feat(core): TaskManager, TaskIntake, crash recovery`

---

### Task 8: engine-http — probe + tek bağlantılı indirme

**Files:**
- Create: `crates/engine-http/src/lib.rs`, `crates/engine-http/src/probe.rs`, `crates/engine-http/src/download.rs`
- Modify: `crates/engine-http/Cargo.toml` (deps: layer-engine-api path, reqwest, tokio, tokio-util, futures, tracing, thiserror)
- Test: `crates/engine-http/tests/single_connection.rs` (wiremock)

**Interfaces:**
- Consumes: `HttpEngine`, `HttpTaskSpec`, `EngineEvent`, `EngineError`, `Progress` (engine-api)
- Produces:
```rust
pub struct ReqwestHttpEngine { client: reqwest::Client }
impl ReqwestHttpEngine { pub fn new() -> Self; }
#[async_trait] impl HttpEngine for ReqwestHttpEngine { /* run() */ }

// probe.rs
pub struct ProbeResult {
    pub total_bytes: Option<u64>, pub etag: Option<String>, pub last_modified: Option<String>,
    pub range_ok: bool, pub final_url: String, pub content_type: Option<String>,
}
pub async fn probe(client: &reqwest::Client, spec: &HttpTaskSpec) -> Result<ProbeResult, EngineError>;
// Kural: GET url, header Range: bytes=0-0 → 206 ise range_ok=true, total=Content-Range son değeri+1;
// 200 ise range_ok=false, total=Content-Length varsa; body düşürülür (stream kapatılır, ilk byte saklanır).
```
- Tek bağlantı akışı (`download.rs::run_single`): `.part` dosyasına (dest_dir/file_name + ".part") stream write; probe'ta alınan ilk byte'ı da yazar; ~1Hz `Progress`; `speed_limit_bps` varsa `ByteBudget::acquire` her chunk'ta; cancel → erken dönüş (`.part` korunur); tamamlanınca `.part` → final rename (hedef varsa `(1)` son ek politikası M2'de; M1: üzerine yazma, rename önce mevcut final dosyayı silmez → çakışıyorsa `Failed(Io)`).
- `run()` orkestrasyonu: `probe` → `EngineEvent::Probed` → range_ok && total biliniyor && max_segments>1 → Task 9 (segmented); değilse `run_single` → `Done`.

- [ ] **Step 1: wiremock testlerini yaz**
  - Range desteklemeyen sunucu (her zaman 200, Content-Length var): probe range_ok=false; indirme tamamlanır; dosya içeriği doğru; Probed+Progress+Done event sırası
  - Content-Length'siz (chunked): total_bytes=None, tek bağlantı, içerik doğru
  - cancel: indirme ortasında token cancel → run döner, `.part` diskte, final dosya YOK
  - hız limiti: 64KB dosya, limit 32KB/s → süre ≥ ~1s (toleranslı)
  - 500 hatası: Failed(Http{status:500}) eventi (retry Task 10'da; bu testte server hep 500 → retry'lar tükenir)
- [ ] **Step 2: FAIL → 3. implement → 4. PASS + clippy → 5. Commit** — `feat(engine-http): probe + single-connection download`

---

### Task 9: engine-http — segmented indirme + resume + If-Range conflict

**Files:**
- Create: `crates/engine-http/src/segmented.rs`
- Test: `crates/engine-http/tests/segmented.rs` (wiremock)

**Interfaces:**
- Consumes: Task 8 `ProbeResult`, `ReqwestHttpEngine`
- Produces:
```rust
// segmented.rs
pub struct SegmentPlan { pub segments: Vec<SegmentProgress> } // end = inclusive son byte offset
pub fn plan(total: u64, max_segments: usize, resume: Option<&ResumeState>) -> SegmentPlan;
// fresh: [0,total) max_segments parçaya (min segment 256KB; küçük dosya → daha az segment)
// resume: mevcut segment sınırları korunur, downloaded offset'lerden devam
pub async fn run_segmented(
    client: &reqwest::Client, spec: &HttpTaskSpec, probe: &ProbeResult, plan: SegmentPlan,
    events: &mpsc::UnboundedSender<EngineEvent>, cancel: &CancellationToken, budget: Option<Arc<ByteBudget>>,
) -> Result<SegmentedOutcome, EngineError>;
pub enum SegmentedOutcome { Completed, Conflict, Cancelled, NeedReResolve }
```
- `.part` dosyası `set_len(total)` ile preallocate; her segment `File::try_clone` + `seek(Start(start+downloaded))` ile kendi bölgesine yazar.
- Segment isteği: `Range: bytes={start+downloaded}-{end}` + (resume veya etag/last-modified biliniyorsa) `If-Range: {etag|last_modified}` → **200 dönerse = kaynak değişti** → `ConflictDetected` emit, tüm segmentler durdurulur, outcome Conflict (`.part` korunur, birleştirilmez).
- Başlangıçta `SegmentPlan` eventi; indirme sırasında ~1s'de bir toplu `SegmentUpdate` (her segment için downloaded).
- Tüm segmentler biter → `Done`.
- Progress: paylaşılan `AtomicU64` toplam downloaded; ~1Hz Progress eventi.
- 403/410 herhangi bir segmentte → `NeedReResolve` emit + diğer segmentleri durdur (cancel token iç) → outcome NeedReResolve.

- [ ] **Step 1: wiremock test helper: Range isteklerini doğru karşılayan mock sunucu** (Content-Range/206, ETag sabit, byte-exact body)
- [ ] **Step 2: Testler**
  - 1MB dosya, 4 segment: içerik byte-byte doğru; SegmentPlan eventi 4 parça; tüm Range istekleri 206
  - max_segments=8 ama dosya 100KB → segment sayısı sınırlanır (min 256KB kuralı)
  - resume: yarım ResumeState (segment 0 tam, segment 1 yarı) → sadece eksik byte'lar istenir (mock'a gelen Range başlıkları doğrulanır), içerik doğru
  - conflict: mock ETag değiştirir + If-Range'e 200 döner → ConflictDetected eventi, outcome Conflict, `.part` duruyor
  - segment isteğine 403 → NeedReResolve
- [ ] **Step 3: FAIL → 4. implement → 5. PASS + clippy → 6. Commit** — `feat(engine-http): segmented download, resume, If-Range conflict`

---

### Task 10: engine-http — retry/backoff

**Files:**
- Create: `crates/engine-http/src/retry.rs`
- Test: `crates/engine-http/tests/retry.rs` (wiremock)

**Interfaces:**
- Produces:
```rust
pub struct RetryPolicy { pub max_attempts: u32, pub base: Duration, pub cap: Duration } // default {5, 500ms, 8s}
pub fn is_transient(err: &reqwest::Error) -> bool; // connect/timeout/body hataları true; 4xx false (403/410 hariç → NeedReResolve zaten ayrı yol)
pub async fn sleep_backoff(attempt: u32, policy: &RetryPolicy, cancel: &CancellationToken);
```
- Uygulama: `run_single` ve segment indirmeleri transient hatada `max_attempts`'e kadar `sleep_backoff` ile yeniden dener; HTTP 5xx transient sayılır; denemeler tükenince `Failed(Network/Http)`.
- 403/410 retry EDİLMEZ → doğrudan `NeedReResolve` (Task 9 ile tutarlı).

- [ ] **Step 1: Testler** — mock: ilk 2 istek 503, 3.'te 200 → indirme başarılı (toplam 3 istek atıldığı wiremock assert'i ile); hep 503 → Failed(Http{503}) ve istek sayısı == max_attempts; connection reset simülasyonu (mock kapatma) → transient retry.
- [ ] **Step 2: FAIL → 3. implement → 4. PASS + clippy → 5. Commit** — `feat(engine-http): retry with exponential backoff`

---

### Task 11: layer-cli — composition root + uçtan uca smoke

**Files:**
- Modify: `apps/cli/Cargo.toml` (deps: layer-core, layer-engine-api, layer-engine-http, tokio, clap)
- Modify: `apps/cli/src/main.rs`
- Create: `apps/cli/src/sink.rs` (stdout EventSink)

**Interfaces:**
- Consumes: `TaskManager::new/start/intake/pause/resume/cancel/shutdown`, `ReqwestHttpEngine`, `Db::open`
- Produces:
```
layer-cli --db <path> add <url> [--dir <path>]        # intake + progress stdout + Completed'de exit 0
layer-cli --db <path> list
layer-cli --db <path> pause|resume|cancel <task-id>
```
`StdoutSink: EventSink` → CoreEvent'leri tek satır log olarak basar. `add`: manager.start → intake → Progress event'leri `\r` ile tek satır güncellenir → Completed/Failed'de çıkış (Failed → exit 1).

- [ ] **Step 1: `main.rs` + `sink.rs` yaz (clap subcommand)**
- [ ] **Step 2: Manuel smoke:** lokal test sunucusu (wiremock tabanlı küçük bir example binary veya `python -m http.server` ile büyük dosya) → `layer-cli add http://127.0.0.1:PORT/big.bin --dir %TEMP%` → dosya iniyor, progress akıyor, Completed; ikinci terminalden `pause`/`resume` çalışıyor; `.part` resume sonrası içerik sağlam (hash karşılaştır)
- [ ] **Step 3: `cargo test --workspace` + `cargo clippy --workspace -- -D warnings` + `cargo fmt --all` → hepsi yeşil**
- [ ] **Step 4: Commit** — `feat(cli): composition root smoke binary`
- [ ] **Step 5: Tag** — M1 bitti: README'ye durum notu (`M1 ✅ headless core+http`), commit `docs: M1 status`

---

## Self-Review Notları

- **Spec coverage:** §3.1 (probe, host-politikası kısmi: max_segments settings'ten, host override tablosu M2'de settings UI ile; fallback tek-bağlantı Task 8-9), §3.2 → M4 (resolver yok; M1'de NeedReResolve→Failed(ResolverRequired) köprüsü Task 7'de), §4 core/engine-http testleri Task 4-10, §5 M0+M1 tamamı. §3.3-3.6 → M3/M4/M5 planlarında.
- **Placeholder taraması:** Task 8'de "final dosya çakışma politikası M2'ye ertelendi" açıkça belirtildi; bilinçli erteleme, placeholder değil.
- **Tip tutarlılığı:** `SegmentedOutcome` (Task 9) ↔ `run()` orkestrasyonu (Task 8) ↔ event isimleri Task 2 ile birebir; `CoreError` Task 7'de tanımlı, Task 11 aynı imzaları kullanır.
