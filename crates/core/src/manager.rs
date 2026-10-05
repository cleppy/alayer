use crate::db::Db;
use crate::error::CoreError;
use crate::intake::classify_url;
use crate::queue::QueuePolicy;
use crate::task::Task;
use layer_engine_api::{
    CoreEvent, EngineEvent, EventSink, FailureReason, HttpEngine, HttpTaskSpec, IntakeSource,
    ResumeState, TaskId, TaskKind, TaskState,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, Notify};
use tokio_util::sync::CancellationToken;

/// Ayar anahtarı: görev başına varsayılan segment sayısı.
const SETTING_MAX_SEGMENTS: &str = "max_segments";
const DEFAULT_MAX_SEGMENTS: usize = 8;

struct Inner {
    db: Arc<Db>,
    http: Arc<dyn HttpEngine>,
    sink: Arc<dyn EventSink>,
    policy: QueuePolicy,
    /// Çalışan görevlerin cancel token'ları. pause/cancel/shutdown buradan keser.
    running: Mutex<HashMap<TaskId, CancellationToken>>,
    /// Kuyrukta değişiklik olduğunda pump'ı uyandırır (notify_one: permit birikir).
    notify: Arc<Notify>,
    shutdown: CancellationToken,
    pump: Mutex<Option<tokio::task::AbortHandle>>,
    dispatched: AtomicUsize,
}

impl Inner {
    fn emit(&self, event: CoreEvent) {
        self.sink.emit(event);
    }

    fn lock_running(&self) -> std::sync::MutexGuard<'_, HashMap<TaskId, CancellationToken>> {
        self.running.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Durum geçişi + kalıcılaştırma + event. Geçiş izinsizse yalnız loglanır
    /// (engine event yarışları: ör. Done sonrası geç Failed).
    fn set_state(&self, task: &mut Task, to: TaskState, failure: Option<FailureReason>) {
        let old = task.state;
        if let Err(e) = task.transition(to) {
            tracing::warn!(task = %task.id, "durum geçişi atlandı: {e}");
            return;
        }
        if failure.is_some() {
            task.failure = failure;
        }
        if let Err(e) = self.db.upsert_task(task) {
            tracing::error!(task = %task.id, "durum yazılamadı: {e}");
            return;
        }
        self.emit(CoreEvent::TaskStateChanged {
            task_id: task.id.clone(),
            old,
            new: to,
            failure: task.failure.clone().filter(|_| to == TaskState::Failed),
        });
    }

    fn max_segments(&self) -> usize {
        self.db
            .get_setting(SETTING_MAX_SEGMENTS)
            .ok()
            .flatten()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MAX_SEGMENTS)
    }

    fn build_spec(&self, t: &Task) -> HttpTaskSpec {
        let resume = if !t.segments.is_empty() || t.etag.is_some() || t.last_modified.is_some() {
            Some(ResumeState {
                etag: t.etag.clone(),
                last_modified: t.last_modified.clone(),
                total_bytes: t.total_bytes,
                segments: t.segments.clone(),
            })
        } else {
            None
        };
        HttpTaskSpec {
            task_id: t.id.clone(),
            url: t.url.clone(),
            headers: t.headers.clone(),
            dest_dir: t.dest_dir.clone(),
            file_name: t.file_name.clone(),
            max_segments: self.max_segments(),
            speed_limit_bps: t.speed_limit_bps,
            resume,
        }
    }

    /// Bir görevi Queued → Downloading geçirip engine'i ve event tüketiciyi spawn eder.
    async fn dispatch(self: &Arc<Self>, id: TaskId) -> Result<(), CoreError> {
        let mut t = self
            .db
            .load_task(&id)?
            .ok_or_else(|| CoreError::NotFound(id.clone()))?;
        if t.kind != TaskKind::Http {
            // M1: yalnız HTTP dispatch edilir (Torrent → M3, Media → M4).
            return Ok(());
        }
        if self.lock_running().contains_key(&id) {
            return Ok(()); // zaten koşuyor (double-dispatch emniyeti)
        }
        self.set_state(&mut t, TaskState::Downloading, None);
        if t.state != TaskState::Downloading {
            // Yarış: listeleme ile dispatch arasında görev Paused'a çekilmiş olabilir.
            tracing::debug!(task = %id, "dispatch atlandı (durum yarıştı)");
            return Ok(());
        }
        let spec = self.build_spec(&t);
        let (tx, rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        self.lock_running().insert(id.clone(), cancel.clone());
        self.dispatched.fetch_add(1, Ordering::SeqCst);

        let engine = self.http.clone();
        let run_cancel = cancel.clone();
        let run_id = id.clone();
        tokio::spawn(async move {
            if let Err(e) = engine.run(spec, tx.clone(), run_cancel).await {
                tracing::error!(task = %run_id, "engine hatası: {e}");
                let _ = tx.send(EngineEvent::Failed(FailureReason::Io(e.to_string())));
            }
            // tx burada düşer → kanal kapanır → tüketici run bittiğini görür
        });

        let inner = Arc::clone(self);
        tokio::spawn(consume_engine_events(inner, id, rx));
        Ok(())
    }

    /// Engine run bittiğinde: görev hâlâ Downloading ise motor terminal event
    /// vermeden çıkmış demektir → Failed. Sonra slot boşaltılır ve pump uyandırılır.
    async fn on_run_finished(self: &Arc<Self>, id: TaskId) {
        if let Ok(Some(mut t)) = self.db.load_task(&id) {
            if t.state == TaskState::Downloading {
                tracing::warn!(task = %id, "engine terminal event vermeden çıktı");
                self.set_state(
                    &mut t,
                    TaskState::Failed,
                    Some(FailureReason::Network(
                        "motor terminal event vermeden çıktı".into(),
                    )),
                );
            }
        }
        self.lock_running().remove(&id);
        self.notify.notify_one();
    }
}

/// Engine event akışını işler: DB günceller (tek doğruluk kaynağı) ve CoreEvent yayar.
async fn consume_engine_events(
    inner: Arc<Inner>,
    task_id: TaskId,
    mut rx: mpsc::UnboundedReceiver<EngineEvent>,
) {
    while let Some(ev) = rx.recv().await {
        // Görev iptal edilmiş olabilir (cancel → DB delete) → döngüden çık.
        let mut t = match inner.db.load_task(&task_id) {
            Ok(Some(t)) => t,
            Ok(None) => break,
            Err(e) => {
                tracing::error!(task = %task_id, "event işlenemedi, görev okunamadı: {e}");
                break;
            }
        };
        match ev {
            EngineEvent::Probed {
                total_bytes,
                etag,
                last_modified,
                range_ok,
            } => {
                t.total_bytes = total_bytes;
                t.etag = etag;
                t.last_modified = last_modified;
                t.range_ok = range_ok;
                // Segmentlenebilir değilse (Range yok veya toplam boyut bilinmiyor)
                // duraklatma = baştan başlama.
                t.unresumable = !(range_ok && total_bytes.is_some());
                if let Err(e) = inner.db.upsert_task(&t) {
                    tracing::error!(task = %task_id, "probe yazılamadı: {e}");
                }
            }
            EngineEvent::SegmentPlan(segments) => {
                t.downloaded_bytes = segments.iter().map(|s| s.downloaded).sum();
                t.segments = segments;
                if let Err(e) = inner.db.upsert_task(&t) {
                    tracing::error!(task = %task_id, "segment planı yazılamadı: {e}");
                }
            }
            EngineEvent::SegmentUpdate { idx, downloaded } => {
                if let Some(seg) = t.segments.iter_mut().find(|s| s.idx == idx) {
                    seg.downloaded = downloaded.max(seg.downloaded);
                }
                t.downloaded_bytes = t.segments.iter().map(|s| s.downloaded).sum();
                if let Err(e) = inner.db.upsert_task(&t) {
                    tracing::error!(task = %task_id, "segment update yazılamadı: {e}");
                }
            }
            EngineEvent::Progress(progress) => {
                inner.emit(CoreEvent::Progress {
                    task_id: task_id.clone(),
                    progress,
                });
            }
            EngineEvent::Done => {
                if let Some(total) = t.total_bytes {
                    t.downloaded_bytes = total;
                } else {
                    t.downloaded_bytes = t
                        .segments
                        .iter()
                        .map(|s| s.downloaded)
                        .sum::<u64>()
                        .max(t.downloaded_bytes);
                }
                inner.set_state(&mut t, TaskState::Completed, None);
            }
            EngineEvent::Failed(reason) => {
                inner.set_state(&mut t, TaskState::Failed, Some(reason));
            }
            EngineEvent::ConflictDetected => {
                inner.set_state(&mut t, TaskState::Conflict, None);
                inner.emit(CoreEvent::Conflict {
                    task_id: task_id.clone(),
                });
            }
            EngineEvent::NeedReResolve => {
                // M1: resolver yok → dürüst köprü. M4'te MediaResolver::refresh
                // ile aynı format_id için tek yeniden çözüm yapılacak.
                inner.set_state(&mut t, TaskState::Failed, Some(FailureReason::ResolverRequired));
            }
        }
    }
    inner.on_run_finished(task_id).await;
}

async fn pump_loop(inner: Arc<Inner>) {
    loop {
        tokio::select! {
            _ = inner.shutdown.cancelled() => break,
            _ = inner.notify.notified() => {}
        }
        // Uyandığında mümkün olduğunca çok görev dispatch et.
        loop {
            if inner.shutdown.is_cancelled() {
                return;
            }
            let tasks = match inner.db.list_tasks() {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("pump: görevler listelenemedi: {e}");
                    break;
                }
            };
            let Some(id) = inner.policy.next_http(&tasks) else {
                break;
            };
            if let Err(e) = inner.dispatch(id).await {
                tracing::error!("pump: dispatch hatası: {e}");
                break; // sıcak döngü yerine bir sonraki notify'yi bekle
            }
        }
    }
}

/// Use-case orkestratörü. Domain mantığı burada; Tauri/CLI kabuğu yalnız delege eder.
///
/// Thread-safety: iç durum `Arc<Inner>`; klonlanabilir.
#[derive(Clone)]
pub struct TaskManager {
    inner: Arc<Inner>,
}

impl TaskManager {
    pub fn new(
        db: Arc<Db>,
        http: Arc<dyn HttpEngine>,
        sink: Arc<dyn EventSink>,
        policy: QueuePolicy,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                http,
                sink,
                policy,
                running: Mutex::new(HashMap::new()),
                notify: Arc::new(Notify::new()),
                shutdown: CancellationToken::new(),
                pump: Mutex::new(None),
                dispatched: AtomicUsize::new(0),
            }),
        }
    }

    /// Çökme kurtarması (Downloading → Paused) + pump'ı başlatır. Idempotent.
    pub async fn start(&self) -> Result<(), CoreError> {
        for mut t in self.inner.db.list_tasks()? {
            if t.state == TaskState::Downloading {
                self.inner.set_state(&mut t, TaskState::Paused, None);
            }
        }
        let mut pump = self.inner.pump.lock().unwrap_or_else(|e| e.into_inner());
        if pump.is_none() {
            let h = tokio::spawn(pump_loop(Arc::clone(&self.inner)));
            *pump = Some(h.abort_handle());
        }
        drop(pump);
        self.inner.notify.notify_one();
        Ok(())
    }

    /// TaskIntake kapısı: UI, CLI, clipboard (M5) ve ileride tarayıcı eklentisi
    /// buradan görev ekler. Kaynak yalnız loglanır/event'e yazılır, davranış değiştirmez.
    pub async fn intake(
        &self,
        source: IntakeSource,
        url: String,
        dest_dir: PathBuf,
    ) -> Result<TaskId, CoreError> {
        let url = url.trim().to_string();
        let kind = classify_url(&url)
            .ok_or_else(|| CoreError::InvalidState(format!("desteklenmeyen şema: {url}")))?;
        let task = match kind {
            TaskKind::Http | TaskKind::Media => Task::new_http(url.clone(), dest_dir, None),
            TaskKind::Torrent => Task::new_torrent(url.clone(), dest_dir),
        };
        self.inner.db.upsert_task(&task)?;
        tracing::info!(task = %task.id, ?kind, ?source, "intake");
        self.inner.emit(CoreEvent::TaskAdded {
            task_id: task.id.clone(),
            kind,
            source,
        });
        self.inner.notify.notify_one();
        Ok(task.id)
    }

    /// Downloading → token cancel + Paused; Queued → Paused.
    /// Sıra önemli: önce DB (Paused), sonra cancel — böylece event tüketici
    /// run bittiğinde görevi Paused görür, Failed'ye çekmez.
    pub async fn pause(&self, id: &str) -> Result<(), CoreError> {
        let mut t = self
            .inner
            .db
            .load_task(id)?
            .ok_or_else(|| CoreError::NotFound(id.to_string()))?;
        match t.state {
            TaskState::Downloading | TaskState::Queued => {}
            s => {
                return Err(CoreError::InvalidState(format!(
                    "{s:?} duraklatılamaz"
                )))
            }
        }
        self.inner.set_state(&mut t, TaskState::Paused, None);
        if let Some(token) = self.inner.lock_running().remove(id) {
            token.cancel();
        }
        self.inner.notify.notify_one(); // slot boşaldı
        Ok(())
    }

    /// Paused | Failed | Conflict → Queued. Conflict'te parçalar sıfırlanır
    /// (Task::transition kuralı). Resume state (segmentler, ETag) DB'den spec'e akar.
    pub async fn resume(&self, id: &str) -> Result<(), CoreError> {
        let mut t = self
            .inner
            .db
            .load_task(id)?
            .ok_or_else(|| CoreError::NotFound(id.to_string()))?;
        match t.state {
            TaskState::Paused | TaskState::Failed | TaskState::Conflict => {}
            s => return Err(CoreError::InvalidState(format!("{s:?} sürdürülemez"))),
        }
        self.inner.set_state(&mut t, TaskState::Queued, None);
        self.inner.notify.notify_one();
        Ok(())
    }

    /// Failed/Conflict sonrası yeniden dener (resume ile aynı kapı).
    pub async fn retry(&self, id: &str) -> Result<(), CoreError> {
        self.resume(id).await
    }

    /// Token cancel + DB'den sil + `.part` dosyasını sil + TaskRemoved eventi.
    pub async fn cancel(&self, id: &str) -> Result<(), CoreError> {
        let t = self
            .inner
            .db
            .load_task(id)?
            .ok_or_else(|| CoreError::NotFound(id.to_string()))?;
        if let Some(token) = self.inner.lock_running().remove(id) {
            token.cancel();
        }
        let part = t.part_path();
        match std::fs::remove_file(&part) {
            Ok(()) => tracing::debug!(task = %id, ".part silindi"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(task = %id, ".part silinemedi: {e}"),
        }
        self.inner.db.delete_task(id)?;
        self.inner.emit(CoreEvent::TaskRemoved {
            task_id: id.to_string(),
        });
        self.inner.notify.notify_one();
        Ok(())
    }

    /// Görev hız limiti. M1: kalıcı ayar, çalışan göreve bir sonraki
    /// dispatch'te uygulanır (canlı güncelleme M2 ayarlar UI'ı ile).
    pub async fn set_task_speed_limit(
        &self,
        id: &str,
        bps: Option<u64>,
    ) -> Result<(), CoreError> {
        let mut t = self
            .inner
            .db
            .load_task(id)?
            .ok_or_else(|| CoreError::NotFound(id.to_string()))?;
        t.speed_limit_bps = bps;
        self.inner.db.upsert_task(&t)?;
        Ok(())
    }

    /// Çalışan görevleri Paused'a çeker, pump'ı durdurur.
    pub async fn shutdown(&self) {
        self.inner.shutdown.cancel();
        if let Some(h) = self.inner.pump.lock().unwrap_or_else(|e| e.into_inner()).take() {
            h.abort();
        }
        let ids: Vec<TaskId> = self
            .inner
            .lock_running()
            .drain()
            .map(|(id, token)| {
                token.cancel();
                id
            })
            .collect();
        for id in ids {
            if let Ok(Some(mut t)) = self.inner.db.load_task(&id) {
                if t.state == TaskState::Downloading {
                    self.inner.set_state(&mut t, TaskState::Paused, None);
                }
            }
        }
    }

    /// Kaç görev engine'e verildi (test/telemetri).
    pub fn dispatch_count(&self) -> usize {
        self.inner.dispatched.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::queue::QueuePolicy;
    use crate::task::Task;
    use layer_engine_api::{
        CoreEvent, EngineError, EngineEvent, FailureReason, HttpEngine, HttpTaskSpec, IntakeSource,
        Progress, SegmentProgress, TaskKind, TaskState,
    };
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::{mpsc, Notify};
    use tokio_util::sync::CancellationToken;

    // --- test doubles -------------------------------------------------------

    #[derive(Default, Clone)]
    struct RecordingSink {
        events: Arc<Mutex<Vec<CoreEvent>>>,
    }

    impl EventSink for RecordingSink {
        fn emit(&self, event: CoreEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    impl RecordingSink {
        fn has_added(&self, id: &str, kind: TaskKind, source: IntakeSource) -> bool {
            self.events.lock().unwrap().iter().any(|e| {
                matches!(e, CoreEvent::TaskAdded { task_id, kind: k, source: s }
                    if task_id == id && *k == kind && *s == source)
            })
        }
        fn has_transition(&self, id: &str, from: TaskState, to: TaskState) -> bool {
            self.events.lock().unwrap().iter().any(|e| {
                matches!(e, CoreEvent::TaskStateChanged { task_id, old, new, .. }
                    if task_id == id && *old == from && *new == to)
            })
        }
        fn has_conflict(&self, id: &str) -> bool {
            self.events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, CoreEvent::Conflict { task_id } if task_id == id))
        }
        fn has_removed(&self, id: &str) -> bool {
            self.events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, CoreEvent::TaskRemoved { task_id } if task_id == id))
        }
        fn progress_count(&self, id: &str) -> usize {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| matches!(e, CoreEvent::Progress { task_id, .. } if task_id == id))
                .count()
        }
    }

    struct FakeEngine {
        events: Vec<EngineEvent>,
        hold: bool,
        done_after_release: bool,
        release: Arc<Notify>,
        calls: Arc<Mutex<Vec<HttpTaskSpec>>>,
    }

    impl FakeEngine {
        fn scripted(events: Vec<EngineEvent>) -> Self {
            Self {
                events,
                hold: false,
                done_after_release: false,
                release: Arc::new(Notify::new()),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }
        /// Event'leri yayınlar, sonra cancel veya release bekler.
        fn holding(events: Vec<EngineEvent>, done_after_release: bool) -> Self {
            Self {
                events,
                hold: true,
                done_after_release,
                release: Arc::new(Notify::new()),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait::async_trait]
    impl HttpEngine for FakeEngine {
        async fn run(
            &self,
            spec: HttpTaskSpec,
            events: mpsc::UnboundedSender<EngineEvent>,
            cancel: CancellationToken,
        ) -> Result<(), EngineError> {
            self.calls.lock().unwrap().push(spec);
            for ev in &self.events {
                let _ = events.send(ev.clone());
            }
            if self.hold {
                tokio::select! {
                    _ = cancel.cancelled() => return Ok(()),
                    _ = self.release.notified() => {
                        if self.done_after_release {
                            let _ = events.send(EngineEvent::Done);
                        }
                    }
                }
            }
            Ok(())
        }
    }

    struct Ctx {
        manager: TaskManager,
        db: Arc<Db>,
        sink: RecordingSink,
        dest: PathBuf,
        _dir: tempfile::TempDir,
    }

    fn setup(engine: FakeEngine, policy: QueuePolicy) -> Ctx {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open(&dir.path().join("layer.db")).unwrap());
        let sink = RecordingSink::default();
        let manager = TaskManager::new(
            db.clone(),
            Arc::new(engine),
            Arc::new(sink.clone()),
            policy,
        );
        Ctx {
            manager,
            db,
            sink,
            dest: dir.path().join("dl"),
            _dir: dir,
        }
    }

    fn state_of(ctx: &Ctx, id: &str) -> Option<TaskState> {
        ctx.db.load_task(id).unwrap().map(|t| t.state)
    }

    async fn wait_for(mut f: impl FnMut() -> bool) {
        for _ in 0..300 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("wait_for timeout");
    }

    const URL: &str = "https://example.com/f.bin";

    // --- testler ------------------------------------------------------------

    #[tokio::test]
    async fn intake_creates_queued_task_and_emits() {
        let engine = FakeEngine::scripted(vec![EngineEvent::Done]);
        let ctx = setup(engine, QueuePolicy::default());
        ctx.manager.start().await.unwrap();
        let id = ctx
            .manager
            .intake(IntakeSource::Ui, URL.into(), ctx.dest.clone())
            .await
            .unwrap();
        let t = ctx.db.load_task(&id).unwrap().unwrap();
        assert_eq!(t.kind, TaskKind::Http);
        assert!(ctx.sink.has_added(&id, TaskKind::Http, IntakeSource::Ui));
        assert_eq!(t.file_name, "f.bin");
    }

    #[tokio::test]
    async fn intake_rejects_unknown_scheme() {
        let ctx = setup(FakeEngine::scripted(vec![]), QueuePolicy::default());
        let err = ctx
            .manager
            .intake(IntakeSource::Ui, "ftp://x/y".into(), ctx.dest.clone())
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::InvalidState(_)));
    }

    #[tokio::test]
    async fn magnet_intake_stays_queued_engine_not_called() {
        let engine = FakeEngine::scripted(vec![EngineEvent::Done]);
        let ctx = setup(engine, QueuePolicy::default());
        ctx.manager.start().await.unwrap();
        let id = ctx
            .manager
            .intake(
                IntakeSource::Ui,
                "magnet:?xt=urn:btih:0123456789abcdef".into(),
                ctx.dest.clone(),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(state_of(&ctx, &id), Some(TaskState::Queued));
        assert_eq!(ctx.manager.dispatch_count(), 0);
    }

    #[tokio::test]
    async fn happy_path_completes_and_persists() {
        let engine = FakeEngine::scripted(vec![
            EngineEvent::Probed {
                total_bytes: Some(1000),
                etag: Some("W/\"1\"".into()),
                last_modified: None,
                range_ok: true,
            },
            EngineEvent::SegmentPlan(vec![SegmentProgress {
                idx: 0,
                start: 0,
                end: 999,
                downloaded: 0,
            }]),
            EngineEvent::Progress(Progress {
                downloaded_bytes: 1000,
                total_bytes: Some(1000),
                speed_bps: 100.0,
            }),
            EngineEvent::Done,
        ]);
        let calls = engine.calls.clone();
        let ctx = setup(engine, QueuePolicy::default());
        ctx.manager.start().await.unwrap();
        let id = ctx
            .manager
            .intake(IntakeSource::Ui, URL.into(), ctx.dest.clone())
            .await
            .unwrap();
        wait_for(|| state_of(&ctx, &id) == Some(TaskState::Completed)).await;

        let t = ctx.db.load_task(&id).unwrap().unwrap();
        assert_eq!(t.total_bytes, Some(1000));
        assert_eq!(t.etag.as_deref(), Some("W/\"1\""));
        assert!(t.range_ok);
        assert!(!t.unresumable);
        assert_eq!(t.downloaded_bytes, 1000);

        assert!(ctx.sink.has_transition(&id, TaskState::Queued, TaskState::Downloading));
        assert!(ctx.sink.has_transition(&id, TaskState::Downloading, TaskState::Completed));
        assert_eq!(ctx.sink.progress_count(&id), 1);

        let spec = calls.lock().unwrap()[0].clone();
        assert_eq!(spec.url, URL);
        assert_eq!(spec.max_segments, 8); // settings yok → varsayılan
        assert!(spec.resume.is_none());
        assert!(spec.headers.is_empty());
    }

    #[tokio::test]
    async fn pause_then_resume_carries_resume_state() {
        let engine = FakeEngine::holding(
            vec![
                EngineEvent::Probed {
                    total_bytes: Some(2000),
                    etag: Some("W/\"e\"".into()),
                    last_modified: Some("Wed, 21 Oct 2025 07:28:00 GMT".into()),
                    range_ok: true,
                },
                EngineEvent::SegmentPlan(vec![
                    SegmentProgress {
                        idx: 0,
                        start: 0,
                        end: 999,
                        downloaded: 400,
                    },
                    SegmentProgress {
                        idx: 1,
                        start: 1000,
                        end: 1999,
                        downloaded: 0,
                    },
                ]),
            ],
            false,
        );
        let calls = engine.calls.clone();
        let ctx = setup(engine, QueuePolicy::default());
        ctx.manager.start().await.unwrap();
        let id = ctx
            .manager
            .intake(IntakeSource::Ui, URL.into(), ctx.dest.clone())
            .await
            .unwrap();

        // Segment planı DB'ye işlenene kadar bekle (pause resume state'i DB'den kurar)
        wait_for(|| {
            ctx.db
                .load_task(&id)
                .unwrap()
                .is_some_and(|t| t.segments.len() == 2 && t.state == TaskState::Downloading)
        })
        .await;

        ctx.manager.pause(&id).await.unwrap();
        wait_for(|| state_of(&ctx, &id) == Some(TaskState::Paused)).await;
        assert!(ctx.sink.has_transition(&id, TaskState::Downloading, TaskState::Paused));

        ctx.manager.resume(&id).await.unwrap();
        wait_for(|| calls.lock().unwrap().len() == 2).await;

        let spec2 = calls.lock().unwrap()[1].clone();
        let resume = spec2.resume.expect("resume state gelmeli");
        assert_eq!(resume.etag.as_deref(), Some("W/\"e\""));
        assert_eq!(resume.total_bytes, Some(2000));
        assert_eq!(resume.segments.len(), 2);
        assert_eq!(resume.segments[0].downloaded, 400);
        assert!(ctx.sink.has_transition(&id, TaskState::Paused, TaskState::Queued));
    }

    #[tokio::test]
    async fn pause_of_queued_task_holds_it() {
        let engine = FakeEngine::holding(vec![], false);
        let ctx = setup(engine, QueuePolicy::new(1));
        // pump başlatılmadan intake → Queued; pause → engine hiç çağrılmadan Paused
        let id = ctx
            .manager
            .intake(IntakeSource::Ui, URL.into(), ctx.dest.clone())
            .await
            .unwrap();
        ctx.manager.pause(&id).await.unwrap();
        assert_eq!(state_of(&ctx, &id), Some(TaskState::Paused));
        ctx.manager.start().await.unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(state_of(&ctx, &id), Some(TaskState::Paused));
        assert_eq!(ctx.manager.dispatch_count(), 0);
    }

    #[tokio::test]
    async fn conflict_marks_state_and_emits() {
        let engine = FakeEngine::scripted(vec![
            EngineEvent::Probed {
                total_bytes: Some(10),
                etag: None,
                last_modified: None,
                range_ok: true,
            },
            EngineEvent::ConflictDetected,
        ]);
        let ctx = setup(engine, QueuePolicy::default());
        ctx.manager.start().await.unwrap();
        let id = ctx
            .manager
            .intake(IntakeSource::Ui, URL.into(), ctx.dest.clone())
            .await
            .unwrap();
        wait_for(|| state_of(&ctx, &id) == Some(TaskState::Conflict)).await;
        assert!(ctx.sink.has_conflict(&id));
    }

    #[tokio::test]
    async fn need_reresolve_bridges_to_resolver_required_failure() {
        let engine = FakeEngine::scripted(vec![EngineEvent::NeedReResolve]);
        let ctx = setup(engine, QueuePolicy::default());
        ctx.manager.start().await.unwrap();
        let id = ctx
            .manager
            .intake(IntakeSource::Ui, URL.into(), ctx.dest.clone())
            .await
            .unwrap();
        wait_for(|| state_of(&ctx, &id) == Some(TaskState::Failed)).await;
        let t = ctx.db.load_task(&id).unwrap().unwrap();
        assert_eq!(t.failure, Some(FailureReason::ResolverRequired));
    }

    #[tokio::test]
    async fn failure_then_retry_reruns_engine() {
        let engine = FakeEngine::scripted(vec![EngineEvent::Failed(FailureReason::Network(
            "conn reset".into(),
        ))]);
        let calls = engine.calls.clone();
        let ctx = setup(engine, QueuePolicy::default());
        ctx.manager.start().await.unwrap();
        let id = ctx
            .manager
            .intake(IntakeSource::Ui, URL.into(), ctx.dest.clone())
            .await
            .unwrap();
        wait_for(|| state_of(&ctx, &id) == Some(TaskState::Failed)).await;
        let t = ctx.db.load_task(&id).unwrap().unwrap();
        assert!(matches!(t.failure, Some(FailureReason::Network(_))));

        ctx.manager.retry(&id).await.unwrap();
        wait_for(|| calls.lock().unwrap().len() == 2).await;
        wait_for(|| state_of(&ctx, &id) == Some(TaskState::Failed)).await;
        assert!(ctx.sink.has_transition(&id, TaskState::Failed, TaskState::Queued));
    }

    #[tokio::test]
    async fn max_concurrent_releases_slot_on_completion() {
        let engine = FakeEngine::holding(vec![], true); // release → Done
        let release = engine.release.clone();
        let ctx = setup(engine, QueuePolicy::new(1));
        ctx.manager.start().await.unwrap();
        let id1 = ctx
            .manager
            .intake(IntakeSource::Ui, "https://e.com/a".into(), ctx.dest.clone())
            .await
            .unwrap();
        let id2 = ctx
            .manager
            .intake(IntakeSource::Ui, "https://e.com/b".into(), ctx.dest.clone())
            .await
            .unwrap();

        wait_for(|| state_of(&ctx, &id1) == Some(TaskState::Downloading)).await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(state_of(&ctx, &id2), Some(TaskState::Queued), "slot dolu");

        release.notify_one(); // id1 → Done → Completed → slot boşalır
        wait_for(|| state_of(&ctx, &id1) == Some(TaskState::Completed)).await;
        wait_for(|| state_of(&ctx, &id2) == Some(TaskState::Downloading)).await;
    }

    #[tokio::test]
    async fn start_recovers_stranded_downloading_to_paused() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open(&dir.path().join("layer.db")).unwrap());
        let mut stranded = Task::new_http(URL.into(), dir.path().join("dl"), None);
        stranded.transition(TaskState::Downloading).unwrap();
        db.upsert_task(&stranded).unwrap();

        let sink = RecordingSink::default();
        let manager = TaskManager::new(
            db.clone(),
            Arc::new(FakeEngine::scripted(vec![])),
            Arc::new(sink.clone()),
            QueuePolicy::default(),
        );
        manager.start().await.unwrap();
        wait_for(|| {
            db.load_task(&stranded.id)
                .unwrap()
                .is_some_and(|t| t.state == TaskState::Paused)
        })
        .await;
        assert!(sink.has_transition(&stranded.id, TaskState::Downloading, TaskState::Paused));
    }

    #[tokio::test]
    async fn cancel_deletes_task_part_file_and_emits() {
        let engine = FakeEngine::holding(vec![], false);
        let ctx = setup(engine, QueuePolicy::default());
        ctx.manager.start().await.unwrap();
        let id = ctx
            .manager
            .intake(IntakeSource::Ui, URL.into(), ctx.dest.clone())
            .await
            .unwrap();
        wait_for(|| state_of(&ctx, &id) == Some(TaskState::Downloading)).await;

        std::fs::create_dir_all(&ctx.dest).unwrap();
        let t = ctx.db.load_task(&id).unwrap().unwrap();
        std::fs::write(t.part_path(), b"partial bytes").unwrap();

        ctx.manager.cancel(&id).await.unwrap();
        wait_for(|| ctx.db.load_task(&id).unwrap().is_none()).await;
        assert!(!t.part_path().exists(), ".part silinmeli");
        assert!(ctx.sink.has_removed(&id));
    }

    #[tokio::test]
    async fn engine_error_maps_to_failed_io() {
        struct BrokenEngine;
        #[async_trait::async_trait]
        impl HttpEngine for BrokenEngine {
            async fn run(
                &self,
                _spec: HttpTaskSpec,
                _events: mpsc::UnboundedSender<EngineEvent>,
                _cancel: CancellationToken,
            ) -> Result<(), EngineError> {
                Err(EngineError::InvalidSpec("bozuk".into()))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open(&dir.path().join("layer.db")).unwrap());
        let sink = RecordingSink::default();
        let manager = TaskManager::new(
            db.clone(),
            Arc::new(BrokenEngine),
            Arc::new(sink.clone()),
            QueuePolicy::default(),
        );
        manager.start().await.unwrap();
        let id = manager
            .intake(IntakeSource::Cli, URL.into(), dir.path().join("dl"))
            .await
            .unwrap();
        wait_for(|| {
            db.load_task(&id)
                .unwrap()
                .is_some_and(|t| t.state == TaskState::Failed)
        })
        .await;
        let t = db.load_task(&id).unwrap().unwrap();
        assert!(matches!(t.failure, Some(FailureReason::Io(_))));
    }

    #[tokio::test]
    async fn shutdown_pauses_running_and_stops_pump() {
        let engine = FakeEngine::holding(vec![], false);
        let calls = engine.calls.clone();
        let ctx = setup(engine, QueuePolicy::default());
        ctx.manager.start().await.unwrap();
        let id = ctx
            .manager
            .intake(IntakeSource::Ui, URL.into(), ctx.dest.clone())
            .await
            .unwrap();
        wait_for(|| state_of(&ctx, &id) == Some(TaskState::Downloading)).await;

        ctx.manager.shutdown().await;
        wait_for(|| state_of(&ctx, &id) == Some(TaskState::Paused)).await;

        // pump öldü: yeni intake dispatch edilmez
        let id2 = ctx
            .manager
            .intake(IntakeSource::Ui, "https://e.com/second".into(), ctx.dest.clone())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(state_of(&ctx, &id2), Some(TaskState::Queued));
        assert_eq!(calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn invalid_transitions_rejected() {
        let ctx = setup(FakeEngine::scripted(vec![]), QueuePolicy::default());
        // olmayan görev
        assert!(matches!(
            ctx.manager.pause("yok").await,
            Err(CoreError::NotFound(_))
        ));
        // Completed görev pause edilemez
        let engine_done = FakeEngine::scripted(vec![EngineEvent::Done]);
        let ctx = setup(engine_done, QueuePolicy::default());
        ctx.manager.start().await.unwrap();
        let id = ctx
            .manager
            .intake(IntakeSource::Ui, URL.into(), ctx.dest.clone())
            .await
            .unwrap();
        wait_for(|| state_of(&ctx, &id) == Some(TaskState::Completed)).await;
        assert!(matches!(
            ctx.manager.pause(&id).await,
            Err(CoreError::InvalidState(_))
        ));
        assert!(matches!(
            ctx.manager.resume(&id).await,
            Err(CoreError::InvalidState(_))
        ));
    }
}
