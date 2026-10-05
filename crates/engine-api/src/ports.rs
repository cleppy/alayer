use crate::budget::ByteBudget;
use crate::types::{EngineError, EngineEvent, FailureReason, HttpTaskSpec, Progress, TaskId, TaskKind, TaskState};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntakeSource {
    Ui,
    Clipboard,
    Cli,
    Extension,
}

/// Core → dış dünya olayları. Kabuk (CLI stdout / Tauri event) bunları kendi
/// biçimine çevirir; throttle gibi sunum detayları kabuğun işidir, core'un değil.
#[derive(Debug, Clone)]
pub enum CoreEvent {
    TaskAdded {
        task_id: TaskId,
        kind: TaskKind,
        source: IntakeSource,
    },
    TaskStateChanged {
        task_id: TaskId,
        old: TaskState,
        new: TaskState,
        failure: Option<FailureReason>,
    },
    Progress {
        task_id: TaskId,
        progress: Progress,
    },
    /// Kaynak değişti: parçalar birleştirilemez, kullanıcı kararı gerekir.
    Conflict {
        task_id: TaskId,
    },
    /// Görev iptal edildi ve DB'den silindi (.part dosyası da silinir).
    TaskRemoved {
        task_id: TaskId,
    },
    /// Dosya adı veya başka bir alan değişti; kabuk listeyi tazeler.
    TaskUpdated {
        task_id: TaskId,
    },
    /// Clipboard "sor" modu: yakalanan URL için onay isteği (M5'te kullanılır).
    IntakePrompt {
        url: String,
        source: IntakeSource,
    },
}

/// Dış dünya (UI/kabuk) core'dan olay alır. Implementasyon kabukta yaşar.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: CoreEvent);
}

/// HTTP(S) indirme motoru portu.
///
/// Sözleşme:
/// - `run` görevi sonuna kadar sürer: tamamlanma, conflict, re-resolve ihtiyacı
///   veya iptal (cancel token) ile biter.
/// - pause ve cancel AYNI mekanizmadır (cancel token); ayrımı core yorumlar.
/// - Engine durum saklamaz; resume bilgisi `spec.resume` ile gelir, segment
///   ilerlemesi `EngineEvent::SegmentPlan/SegmentUpdate` ile dışarı verilir.
/// - Kalıcılık engine'in işi DEĞİLDİR — tek doğruluk kaynağı core'un DB'si.
/// - `global_limit` tüm görevlerin paylaştığı hız tavanıdır. `None` = sınırsız.
///   Görev limiti `spec.speed_limit_bps`'tedir; ikisi de varsa yavaş olan kazanır.
#[derive(Debug, Clone)]
pub struct MediaTaskSpec {
    pub task_id: TaskId,
    pub url: String,
    pub dest_dir: std::path::PathBuf,
}

/// Sayfa linkini medyaya çeviren çözücü (yt-dlp bir implementasyondur, core onu tanımaz).
#[async_trait::async_trait]
pub trait MediaResolver: Send + Sync {
    async fn download(
        &self,
        spec: MediaTaskSpec,
        events: mpsc::UnboundedSender<EngineEvent>,
        cancel: CancellationToken,
    ) -> Result<(), EngineError>;
}

#[derive(Debug, Clone)]
pub struct TorrentTaskSpec {
    pub task_id: TaskId,
    /// `magnet:?...` veya `.torrent` dosyasının yolu.
    pub source: String,
    pub dest_dir: std::path::PathBuf,
}

/// BitTorrent motoru. aria2 bir implementasyondur; core onu tanımaz.
#[async_trait::async_trait]
pub trait TorrentEngine: Send + Sync {
    async fn run(
        &self,
        spec: TorrentTaskSpec,
        events: mpsc::UnboundedSender<EngineEvent>,
        cancel: CancellationToken,
    ) -> Result<(), EngineError>;
}

#[async_trait::async_trait]
pub trait HttpEngine: Send + Sync {
    async fn run(
        &self,
        spec: HttpTaskSpec,
        events: mpsc::UnboundedSender<EngineEvent>,
        cancel: CancellationToken,
        global_limit: Option<Arc<ByteBudget>>,
    ) -> Result<(), EngineError>;
}
