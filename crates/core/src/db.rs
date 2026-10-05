use crate::task::Task;
use layer_engine_api::{FailureReason, ResolverContext, SegmentProgress, TaskKind, TaskState};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// SQLite tek doğruluk kaynağı. `.part` dosyaları yalnız byte tutar;
/// görev/segment/doğrulayıcı (ETag, Last-Modified) durumu burada yaşar.
///
/// M1: tek bağlantı + `Mutex` (yazma hacmi düşük: periyodik flush).
pub struct Db {
    conn: Mutex<Connection>,
}

const TASK_COLS: &str = "id, kind, state, url, headers, dest_dir, file_name, total_bytes, \
downloaded_bytes, etag, last_modified, range_ok, unresumable, resolver_ctx, failure, \
speed_limit_bps, segments, created_at, updated_at";

const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL,
    state TEXT NOT NULL,
    url TEXT NOT NULL,
    headers TEXT NOT NULL DEFAULT '[]',
    dest_dir TEXT NOT NULL,
    file_name TEXT NOT NULL,
    total_bytes INTEGER,
    downloaded_bytes INTEGER NOT NULL DEFAULT 0,
    etag TEXT,
    last_modified TEXT,
    range_ok INTEGER NOT NULL DEFAULT 0,
    unresumable INTEGER NOT NULL DEFAULT 0,
    resolver_ctx TEXT,
    failure TEXT,
    speed_limit_bps INTEGER,
    segments TEXT NOT NULL DEFAULT '[]',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tasks_created_at ON tasks(created_at);
";

fn to_i64(v: u64) -> i64 {
    v.try_into().unwrap_or(i64::MAX)
}

fn from_i64(v: i64) -> u64 {
    v.try_into().unwrap_or(u64::MAX)
}

fn json_col<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row,
    idx: usize,
) -> rusqlite::Result<T> {
    let s: String = row.get(idx)?;
    serde_json::from_str(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn json_col_opt<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row,
    idx: usize,
) -> rusqlite::Result<Option<T>> {
    let s: Option<String> = row.get(idx)?;
    match s {
        None => Ok(None),
        Some(s) => serde_json::from_str(&s).map(Some).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
        }),
    }
}

fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<Task> {
    let dest_dir: String = row.get(5)?;
    let range_ok: i64 = row.get(11)?;
    let unresumable: i64 = row.get(12)?;
    let total_bytes: Option<i64> = row.get(7)?;
    let downloaded_bytes: i64 = row.get(8)?;
    let speed_limit_bps: Option<i64> = row.get(15)?;
    Ok(Task {
        id: row.get(0)?,
        kind: json_col::<TaskKind>(row, 1)?,
        state: json_col::<TaskState>(row, 2)?,
        url: row.get(3)?,
        headers: json_col::<Vec<(String, String)>>(row, 4)?,
        dest_dir: PathBuf::from(dest_dir),
        file_name: row.get(6)?,
        total_bytes: total_bytes.map(from_i64),
        downloaded_bytes: from_i64(downloaded_bytes),
        etag: row.get(9)?,
        last_modified: row.get(10)?,
        range_ok: range_ok != 0,
        unresumable: unresumable != 0,
        resolver_ctx: json_col_opt::<ResolverContext>(row, 13)?,
        failure: json_col_opt::<FailureReason>(row, 14)?,
        speed_limit_bps: speed_limit_bps.map(from_i64),
        segments: json_col::<Vec<SegmentProgress>>(row, 16)?,
        created_at: row.get(17)?,
        updated_at: row.get(18)?,
    })
}

impl Db {
    /// `":memory:"` desteklenir (testler); aksi halde dosya yolu (parent dizin oluşturulur).
    pub fn open(path: &Path) -> Result<Self, DbError> {
        let conn = if path.to_str() == Some(":memory:") {
            Connection::open_in_memory()?
        } else {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            Connection::open(path)?
        };
        conn.busy_timeout(Duration::from_secs(5))?;
        Self::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn migrate(conn: &Connection) -> Result<(), DbError> {
        let version: i64 = conn
            .query_row("PRAGMA user_version", params![], |r| r.get(0))
            .unwrap_or(0);
        if version < 1 {
            conn.execute_batch(SCHEMA_V1)?;
            conn.pragma_update(None, "user_version", 1)?;
        }
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn upsert_task(&self, t: &Task) -> Result<(), DbError> {
        let conn = self.lock();
        let kind = serde_json::to_string(&t.kind)?;
        let state = serde_json::to_string(&t.state)?;
        let headers = serde_json::to_string(&t.headers)?;
        let resolver_ctx = match &t.resolver_ctx {
            Some(rc) => Some(serde_json::to_string(rc)?),
            None => None,
        };
        let failure = match &t.failure {
            Some(f) => Some(serde_json::to_string(f)?),
            None => None,
        };
        let segments = serde_json::to_string(&t.segments)?;
        let dest_dir = t.dest_dir.to_string_lossy().to_string();

        let updates: Vec<String> = TASK_COLS
            .split(", ")
            .filter(|c| *c != "id")
            .map(|c| format!("{c}=excluded.{c}"))
            .collect();
        let placeholders: Vec<String> = (0..19).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "INSERT INTO tasks ({TASK_COLS}) VALUES ({}) ON CONFLICT(id) DO UPDATE SET {}",
            placeholders.join(", "),
            updates.join(", ")
        );

        conn.execute(
            &sql,
            params![
                t.id,
                kind,
                state,
                t.url,
                headers,
                dest_dir,
                t.file_name,
                t.total_bytes.map(to_i64),
                to_i64(t.downloaded_bytes),
                t.etag,
                t.last_modified,
                t.range_ok as i64,
                t.unresumable as i64,
                resolver_ctx,
                failure,
                t.speed_limit_bps.map(to_i64),
                segments,
                t.created_at,
                t.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn load_task(&self, id: &str) -> Result<Option<Task>, DbError> {
        let conn = self.lock();
        let sql = format!("SELECT {TASK_COLS} FROM tasks WHERE id = ?1");
        let task = conn.query_row(&sql, params![id], row_to_task).optional()?;
        Ok(task)
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>, DbError> {
        let conn = self.lock();
        let sql = format!("SELECT {TASK_COLS} FROM tasks ORDER BY created_at ASC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_task)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Yalnız bayt alanları. Durum yazısı bunu ezemez.
    pub fn set_progress(
        &self,
        id: &str,
        downloaded: u64,
        total: Option<u64>,
    ) -> Result<(), DbError> {
        let conn = self.lock();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        if let Some(total) = total {
            conn.execute(
                "UPDATE tasks SET downloaded_bytes = MAX(downloaded_bytes, ?1), total_bytes = CASE WHEN total_bytes IS NULL OR total_bytes < ?2 THEN ?2 ELSE total_bytes END, updated_at = ?3 WHERE id = ?4",
                params![to_i64(downloaded), to_i64(total), now, id],
            )?;
        } else {
            conn.execute(
                "UPDATE tasks SET downloaded_bytes = MAX(downloaded_bytes, ?1), updated_at = ?2 WHERE id = ?3",
                params![to_i64(downloaded), now, id],
            )?;
        }
        Ok(())
    }

    pub fn set_file_name(&self, id: &str, name: &str) -> Result<(), DbError> {
        let conn = self.lock();
        conn.execute(
            "UPDATE tasks SET file_name = ?1 WHERE id = ?2",
            params![name, id],
        )?;
        Ok(())
    }

    /// Idempotent: olmayan id için Ok(0).
    pub fn delete_task(&self, id: &str) -> Result<(), DbError> {
        let conn = self.lock();
        conn.execute("DELETE FROM tasks WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, DbError> {
        let conn = self.lock();
        let v = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(v)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), DbError> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::Task;
    use layer_engine_api::{FailureReason, ResolverContext, SegmentProgress, TaskKind, TaskState};
    use std::path::PathBuf;

    fn sample_task() -> Task {
        let mut t = Task::new_http(
            "https://example.com/f.bin".into(),
            PathBuf::from("C:/dl"),
            None,
        );
        t.headers = vec![("User-Agent".into(), "layer".into())];
        t.total_bytes = Some(1000);
        t.downloaded_bytes = 400;
        t.etag = Some("W/\"xyz\"".into());
        t.last_modified = Some("Wed, 21 Oct 2025 07:28:00 GMT".into());
        t.range_ok = true;
        t.speed_limit_bps = Some(2048);
        t.segments = vec![
            SegmentProgress {
                idx: 0,
                start: 0,
                end: 499,
                downloaded: 400,
            },
            SegmentProgress {
                idx: 1,
                start: 500,
                end: 999,
                downloaded: 0,
            },
        ];
        t.resolver_ctx = Some(ResolverContext {
            original_url: "https://youtu.be/x".into(),
            extractor: "youtube".into(),
            format_id: "137".into(),
            expected_size: Some(1000),
            audio_format: None,
        });
        t
    }

    fn mem_db() -> Db {
        Db::open(std::path::Path::new(":memory:")).unwrap()
    }

    #[test]
    fn roundtrip_all_fields() {
        let db = mem_db();
        let t = sample_task();
        db.upsert_task(&t).unwrap();
        let back = db.load_task(&t.id).unwrap().expect("yok");
        assert_eq!(back.id, t.id);
        assert_eq!(back.kind, TaskKind::Http);
        assert_eq!(back.url, t.url);
        assert_eq!(back.headers, t.headers);
        assert_eq!(back.dest_dir, t.dest_dir);
        assert_eq!(back.file_name, "f.bin");
        assert_eq!(back.total_bytes, Some(1000));
        assert_eq!(back.downloaded_bytes, 400);
        assert_eq!(back.etag.as_deref(), Some("W/\"xyz\""));
        assert_eq!(back.last_modified, t.last_modified);
        assert!(back.range_ok);
        assert!(!back.unresumable);
        assert_eq!(back.speed_limit_bps, Some(2048));
        assert_eq!(back.segments.len(), 2);
        assert_eq!(back.segments[0].downloaded, 400);
        assert_eq!(back.segments[1].end, 999);
        let rc = back.resolver_ctx.unwrap();
        assert_eq!(rc.extractor, "youtube");
        assert_eq!(rc.format_id, "137");
        assert_eq!(back.created_at, t.created_at);
    }

    #[test]
    fn load_missing_returns_none() {
        let db = mem_db();
        assert!(db.load_task("nope").unwrap().is_none());
    }

    #[test]
    fn upsert_updates_in_place() {
        let db = mem_db();
        let mut t = sample_task();
        db.upsert_task(&t).unwrap();
        t.state = TaskState::Failed;
        t.failure = Some(FailureReason::Http {
            status: 500,
            message: "ise".into(),
        });
        t.downloaded_bytes = 700;
        t.segments[0].downloaded = 500;
        db.upsert_task(&t).unwrap();

        let back = db.load_task(&t.id).unwrap().unwrap();
        assert_eq!(back.state, TaskState::Failed);
        assert_eq!(
            back.failure,
            Some(FailureReason::Http {
                status: 500,
                message: "ise".into()
            })
        );
        assert_eq!(back.downloaded_bytes, 700);
        assert_eq!(back.segments[0].downloaded, 500);
        assert_eq!(db.list_tasks().unwrap().len(), 1, "upsert duplike etmemeli");
    }

    #[test]
    fn delete_removes_task() {
        let db = mem_db();
        let t = sample_task();
        db.upsert_task(&t).unwrap();
        db.delete_task(&t.id).unwrap();
        assert!(db.load_task(&t.id).unwrap().is_none());
    }

    #[test]
    fn list_ordered_by_created_at() {
        let db = mem_db();
        let a = sample_task();
        let mut b = sample_task();
        b.created_at = a.created_at + 1000;
        db.upsert_task(&b).unwrap();
        db.upsert_task(&a).unwrap();
        let list = db.list_tasks().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, a.id, "eski önce");
        assert_eq!(list[1].id, b.id);
    }

    #[test]
    fn settings_get_set_overwrite() {
        let db = mem_db();
        assert_eq!(db.get_setting("max_segments").unwrap(), None);
        db.set_setting("max_segments", "8").unwrap();
        assert_eq!(
            db.get_setting("max_segments").unwrap().as_deref(),
            Some("8")
        );
        db.set_setting("max_segments", "4").unwrap();
        assert_eq!(
            db.get_setting("max_segments").unwrap().as_deref(),
            Some("4")
        );
    }

    #[test]
    fn file_db_persists_across_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layer.db");
        let t = sample_task();
        {
            let db = Db::open(&path).unwrap();
            db.upsert_task(&t).unwrap();
            db.set_setting("k", "v").unwrap();
        }
        {
            let db = Db::open(&path).unwrap();
            let back = db.load_task(&t.id).unwrap().expect("kalıcı olmalı");
            assert_eq!(back.downloaded_bytes, 400);
            assert_eq!(db.get_setting("k").unwrap().as_deref(), Some("v"));
        }
    }
}
