use std::path::PathBuf;

/// Kalıcı görev kimliği (uuid v4). GID gibi geçici tanımlayıcılar kimlik DEĞİLDİR.
pub type TaskId = String;

/// Yeni bir `TaskId` üretir.
pub fn new_task_id() -> TaskId {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Http,
    Torrent,
    Media,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "reason", content = "detail", rename_all = "snake_case")]
pub enum FailureReason {
    Network(String),
    Http { status: u16, message: String },
    /// 403/410 alındı ve resolver yok ya da yeniden çözüm başarısız.
    ResolverRequired,
    /// yt-dlp hata sınıflandırması (M4).
    Resolver(String),
    Io(String),
    /// aria2 hata sınıflandırması (M3).
    Torrent(String),
}

impl FailureReason {
    /// Kullanıcıya gösterilecek tek satırlık mesaj.
    pub fn display(&self) -> String {
        match self {
            FailureReason::Network(m) => format!("ağ hatası: {m}"),
            FailureReason::Http { status, message } => format!("HTTP {status}: {message}"),
            FailureReason::ResolverRequired => {
                "link yenilenmesi gerekiyor ama çözücü yok".to_string()
            }
            FailureReason::Resolver(m) => format!("çözücü hatası: {m}"),
            FailureReason::Io(m) => format!("disk hatası: {m}"),
            FailureReason::Torrent(m) => format!("torrent hatası: {m}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Queued,
    Downloading,
    Paused,
    Completed,
    Failed,
    /// Kaynak değişti (If-Range 200 döndü) — parçalar birleştirilemez.
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Progress {
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub speed_bps: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SegmentProgress {
    pub idx: u32,
    pub start: u64,
    /// Inclusive son byte offset.
    pub end: u64,
    pub downloaded: u64,
}

impl SegmentProgress {
    /// Segmentin toplam byte uzunluğu.
    pub fn byte_len(&self) -> u64 {
        self.end - self.start + 1
    }

    pub fn is_complete(&self) -> bool {
        self.downloaded >= self.byte_len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResumeState {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub total_bytes: Option<u64>,
    pub segments: Vec<SegmentProgress>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HttpTaskSpec {
    pub task_id: TaskId,
    pub url: String,
    /// yt-dlp el değişiminde `http_headers` buraya AYNEN gelir (User-Agent, Referer, Cookie).
    pub headers: Vec<(String, String)>,
    pub dest_dir: PathBuf,
    pub file_name: String,
    pub max_segments: usize,
    pub speed_limit_bps: Option<u64>,
    pub resume: Option<ResumeState>,
}

/// Ölen URL akışında (googlevideo vb.) yeniden çözüm için gereken bağlam.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResolverContext {
    pub original_url: String,
    pub extractor: String,
    pub format_id: String,
    pub expected_size: Option<u64>,
}

/// Engine → core olay akışı (`run()` başına bir mpsc kanalı).
#[derive(Debug, Clone)]
pub enum EngineEvent {
    Probed {
        total_bytes: Option<u64>,
        etag: Option<String>,
        last_modified: Option<String>,
        range_ok: bool,
    },
    Progress(Progress),
    /// Segment planı kesinleşti — core DB'ye yazar.
    SegmentPlan(Vec<SegmentProgress>),
    /// Periyodik segment ilerlemesi (flush).
    SegmentUpdate {
        idx: u32,
        downloaded: u64,
    },
    /// Final dosya yerine taşındı.
    Done,
    Failed(FailureReason),
    /// If-Range 200 döndü / kaynak değişti — parçalar birleştirilmez.
    ConflictDetected,
    /// 403/410 — ResolverContext ile tazeleme gerekli (kör retry YASAK).
    NeedReResolve,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("invalid spec: {0}")]
    InvalidSpec(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("internal: {0}")]
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_kind_serde_snake_case() {
        assert_eq!(serde_json::to_string(&TaskKind::Http).unwrap(), "\"http\"");
        assert_eq!(
            serde_json::from_str::<TaskKind>("\"torrent\"").unwrap(),
            TaskKind::Torrent
        );
        assert_eq!(
            serde_json::from_str::<TaskKind>("\"media\"").unwrap(),
            TaskKind::Media
        );
    }

    #[test]
    fn task_state_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&TaskState::Downloading).unwrap(),
            "\"downloading\""
        );
        assert_eq!(
            serde_json::from_str::<TaskState>("\"conflict\"").unwrap(),
            TaskState::Conflict
        );
    }

    #[test]
    fn failure_reason_roundtrip() {
        let r = FailureReason::Http {
            status: 403,
            message: "gone".into(),
        };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"reason\":\"http\""));
        assert_eq!(serde_json::from_str::<FailureReason>(&j).unwrap(), r);

        let r2 = FailureReason::ResolverRequired;
        let j2 = serde_json::to_string(&r2).unwrap();
        assert_eq!(j2, "{\"reason\":\"resolver_required\"}");
        assert_eq!(serde_json::from_str::<FailureReason>(&j2).unwrap(), r2);
    }

    #[test]
    fn resume_state_roundtrip() {
        let rs = ResumeState {
            etag: Some("W/\"abc\"".into()),
            last_modified: None,
            total_bytes: Some(100),
            segments: vec![SegmentProgress {
                idx: 0,
                start: 0,
                end: 99,
                downloaded: 40,
            }],
        };
        let j = serde_json::to_string(&rs).unwrap();
        let back: ResumeState = serde_json::from_str(&j).unwrap();
        assert_eq!(back.segments[0].downloaded, 40);
        assert_eq!(back.etag.as_deref(), Some("W/\"abc\""));
    }

    #[test]
    fn http_task_spec_roundtrip() {
        let spec = HttpTaskSpec {
            task_id: "id-1".into(),
            url: "https://example.com/f.bin".into(),
            headers: vec![("User-Agent".into(), "layer".into())],
            dest_dir: std::path::PathBuf::from("C:/downloads"),
            file_name: "f.bin".into(),
            max_segments: 8,
            speed_limit_bps: Some(1024),
            resume: None,
        };
        let j = serde_json::to_string(&spec).unwrap();
        let back: HttpTaskSpec = serde_json::from_str(&j).unwrap();
        assert_eq!(back.headers[0].0, "User-Agent");
        assert_eq!(back.speed_limit_bps, Some(1024));
    }

    #[test]
    fn resolver_context_roundtrip() {
        let rc = ResolverContext {
            original_url: "https://youtu.be/x".into(),
            extractor: "youtube".into(),
            format_id: "137".into(),
            expected_size: Some(1234),
        };
        let j = serde_json::to_string(&rc).unwrap();
        let back: ResolverContext = serde_json::from_str(&j).unwrap();
        assert_eq!(back.format_id, "137");
    }

    #[test]
    fn new_task_id_is_unique_uuid() {
        let a = new_task_id();
        let b = new_task_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
    }
}
