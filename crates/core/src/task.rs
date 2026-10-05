use layer_engine_api::{
    new_task_id, FailureReason, ResolverContext, SegmentProgress, TaskKind, TaskState,
};
use std::path::PathBuf;

/// İndirme görevi — domain entity'si.
///
/// Kalıcı kimlik `id`'dir (uuid). aria2 GID'i (M3) veya HTTP bağlantı durumu
/// burada kimlik olarak tutulmaz; DB satırı tek doğruluk kaynağıdır.
#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub kind: TaskKind,
    pub state: TaskState,
    /// HTTP(S) URL veya magnet URI.
    pub url: String,
    /// İsteğe eklenecek başlıklar (yt-dlp el değişiminde `http_headers` aynen buraya).
    pub headers: Vec<(String, String)>,
    pub dest_dir: PathBuf,
    pub file_name: String,
    pub total_bytes: Option<u64>,
    pub downloaded_bytes: u64,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub range_ok: bool,
    /// Range desteklenmiyorsa true: duraklatma = baştan başlama.
    pub unresumable: bool,
    pub resolver_ctx: Option<ResolverContext>,
    pub failure: Option<FailureReason>,
    pub speed_limit_bps: Option<u64>,
    pub segments: Vec<SegmentProgress>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, thiserror::Error)]
#[error("illegal transition {from:?} -> {to:?}")]
pub struct InvalidTransition {
    pub from: TaskState,
    pub to: TaskState,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Task {
    pub fn new_http(url: String, dest_dir: PathBuf, file_name: Option<String>) -> Self {
        let name = sanitize_file_name(&file_name.unwrap_or_else(|| derive_file_name(&url)));
        Self::base(TaskKind::Http, url, dest_dir, name)
    }

    /// Magnet/.torrent görevi. Magnet'te gerçek ad metadata ile gelir (M3);
    /// `dn=` parametresi geçici addır.
    pub fn new_torrent(url_or_magnet: String, dest_dir: PathBuf) -> Self {
        let name = magnet_display_name(&url_or_magnet).unwrap_or_else(|| "torrent".into());
        Self::base(TaskKind::Torrent, url_or_magnet, dest_dir, name)
    }

    fn base(kind: TaskKind, url: String, dest_dir: PathBuf, file_name: String) -> Self {
        let now = now_ms();
        Self {
            id: new_task_id(),
            kind,
            state: TaskState::Queued,
            url,
            headers: Vec::new(),
            dest_dir,
            file_name,
            total_bytes: None,
            downloaded_bytes: 0,
            etag: None,
            last_modified: None,
            range_ok: false,
            unresumable: false,
            resolver_ctx: None,
            failure: None,
            speed_limit_bps: None,
            segments: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Durum makinesi. İzinli geçişler:
    /// `Queued→{Downloading,Paused}`, `Downloading→{Paused,Completed,Failed,Conflict}`,
    /// `Paused→Queued`, `Failed→Queued` (retry), `Conflict→Queued` (parçalar sıfırlanır).
    pub fn transition(&mut self, to: TaskState) -> Result<(), InvalidTransition> {
        use TaskState::*;
        let from = self.state;
        let allowed = matches!(
            (from, to),
            (Queued, Downloading)
                | (Queued, Paused)
                | (Downloading, Paused)
                | (Downloading, Completed)
                | (Downloading, Failed)
                | (Downloading, Conflict)
                | (Paused, Queued)
                | (Failed, Queued)
                | (Conflict, Queued)
        );
        if !allowed {
            return Err(InvalidTransition { from, to });
        }
        if from == Conflict && to == Queued {
            // Kaynak değişti — parçalar birleştirilemez, sıfırdan başlanır.
            self.segments.clear();
            self.downloaded_bytes = 0;
            self.etag = None;
            self.last_modified = None;
        }
        if from == Failed && to == Queued {
            self.failure = None;
        }
        self.state = to;
        self.updated_at = now_ms();
        Ok(())
    }

    pub fn is_active(&self) -> bool {
        self.state == TaskState::Downloading
    }

    pub fn is_finished(&self) -> bool {
        self.state == TaskState::Completed
    }

    /// `.part` dosya yolu (byte'ların durduğu yer; metadata DEĞİL).
    pub fn part_path(&self) -> PathBuf {
        self.dest_dir.join(format!("{}.part", self.file_name))
    }

    pub fn final_path(&self) -> PathBuf {
        self.dest_dir.join(&self.file_name)
    }
}

fn derive_file_name(url: &str) -> String {
    let without_query = url.split(['?', '#']).next().unwrap_or("");
    // Scheme + authority atılır: host adı dosya adı DEĞİLDİR.
    // "https://example.com/" → path "/" (boş); "https://example.com" → path yok.
    let path = match without_query.find("://") {
        Some(i) => match without_query[i + 3..].find('/') {
            Some(j) => &without_query[i + 3 + j..],
            None => "",
        },
        None => without_query,
    };
    match path.rsplit('/').find(|s| !s.is_empty()) {
        Some(seg) => {
            let s = sanitize_file_name(seg);
            if s.is_empty() {
                "download".into()
            } else {
                s
            }
        }
        None => "download".into(),
    }
}

fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    // Windows: sondaki nokta/boşluk geçersiz
    let trimmed = cleaned.trim_end_matches([' ', '.']).to_string();
    if trimmed.is_empty() {
        "download".into()
    } else {
        trimmed
    }
}

fn magnet_display_name(uri: &str) -> Option<String> {
    let query = uri.split_once('?').map(|(_, q)| q).unwrap_or("");
    for pair in query.split('&') {
        if let Some(v) = pair.strip_prefix("dn=") {
            if !v.is_empty() {
                return Some(sanitize_file_name(v));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use layer_engine_api::{TaskKind, TaskState};
    use std::path::PathBuf;

    fn http_task() -> Task {
        Task::new_http(
            "https://example.com/files/big.zip?v=2".into(),
            PathBuf::from("C:/downloads"),
            None,
        )
    }

    #[test]
    fn new_http_defaults() {
        let t = http_task();
        assert_eq!(t.kind, TaskKind::Http);
        assert_eq!(t.state, TaskState::Queued);
        assert_eq!(t.file_name, "big.zip"); // query atıldı, path son segment
        assert_eq!(t.id.len(), 36);
        assert_eq!(t.downloaded_bytes, 0);
        assert!(!t.range_ok);
        assert!(!t.unresumable);
    }

    #[test]
    fn file_name_fallback_and_sanitize() {
        let t = Task::new_http("https://example.com/".into(), PathBuf::from("."), None);
        assert_eq!(t.file_name, "download"); // boş path → fallback

        let t2 = Task::new_http(
            "https://example.com/a%20b:c<d>.bin".into(),
            PathBuf::from("."),
            None,
        );
        // Windows-geçersiz karakterler '_' oldu, uzantı korundu
        assert!(!t2.file_name.contains(['<', '>', ':', '"', '|', '?', '*']));
        assert!(t2.file_name.ends_with(".bin"));

        // Açık file_name de sanitize edilir
        let t3 = Task::new_http(
            "https://example.com/x".into(),
            PathBuf::from("."),
            Some("bad:name?.iso".into()),
        );
        assert_eq!(t3.file_name, "bad_name_.iso");
    }

    #[test]
    fn new_torrent_magnet() {
        let t = Task::new_torrent(
            "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=ubuntu".into(),
            PathBuf::from("C:/torrents"),
        );
        assert_eq!(t.kind, TaskKind::Torrent);
        assert_eq!(t.state, TaskState::Queued);
    }

    #[test]
    fn allowed_transitions() {
        let ok = [
            (TaskState::Queued, TaskState::Downloading),
            (TaskState::Queued, TaskState::Paused),
            (TaskState::Downloading, TaskState::Paused),
            (TaskState::Downloading, TaskState::Completed),
            (TaskState::Downloading, TaskState::Failed),
            (TaskState::Downloading, TaskState::Conflict),
            (TaskState::Paused, TaskState::Queued),
            (TaskState::Failed, TaskState::Queued),
            (TaskState::Conflict, TaskState::Queued),
        ];
        for (from, to) in ok {
            let mut t = http_task();
            t.state = from;
            assert!(t.transition(to).is_ok(), "{from:?}->{to:?} izinli olmalı");
            assert_eq!(t.state, to);
        }
    }

    #[test]
    fn illegal_transitions() {
        let bad = [
            (TaskState::Completed, TaskState::Downloading),
            (TaskState::Queued, TaskState::Completed),
            (TaskState::Paused, TaskState::Downloading), // önce Queued'a dönmeli
            (TaskState::Completed, TaskState::Queued),
            (TaskState::Downloading, TaskState::Queued),
            (TaskState::Failed, TaskState::Downloading),
        ];
        for (from, to) in bad {
            let mut t = http_task();
            t.state = from;
            let err = t.transition(to).unwrap_err();
            assert_eq!(err.from, from);
            assert_eq!(err.to, to);
            assert_eq!(t.state, from, "state değişmemeli");
        }
    }

    #[test]
    fn conflict_to_queued_resets_parts() {
        let mut t = http_task();
        t.state = TaskState::Conflict;
        t.downloaded_bytes = 500;
        t.segments = vec![layer_engine_api::SegmentProgress {
            idx: 0,
            start: 0,
            end: 999,
            downloaded: 500,
        }];
        t.transition(TaskState::Queued).unwrap();
        assert_eq!(t.downloaded_bytes, 0);
        assert!(t.segments.is_empty());
        assert!(t.etag.is_none(), "çakışan kaynağın doğrulayıcısı atılmalı");
        assert!(t.last_modified.is_none());
    }

    #[test]
    fn failed_to_queued_clears_failure() {
        let mut t = http_task();
        t.state = TaskState::Failed;
        t.failure = Some(layer_engine_api::FailureReason::Network("x".into()));
        t.transition(TaskState::Queued).unwrap();
        assert!(t.failure.is_none());
    }

    #[test]
    fn state_predicates() {
        let mut t = http_task();
        assert!(!t.is_active());
        t.state = TaskState::Downloading;
        assert!(t.is_active());
        t.state = TaskState::Completed;
        assert!(t.is_finished());
    }
}
