use crate::task::Task;
use layer_engine_api::{TaskId, TaskKind, TaskState};

/// Eşzamanlı görev limiti + FIFO seçim.
///
/// Not: limit yalnız HTTP görevlerini sayar — Torrent (M3) ve Media (M4)
/// motorları geldiğinde kendi eşzamanlılık bütçelerini taşıyacak.
#[derive(Debug, Clone)]
pub struct QueuePolicy {
    pub max_concurrent_http: usize,
}

impl Default for QueuePolicy {
    fn default() -> Self {
        Self {
            max_concurrent_http: 3,
        }
    }
}

impl QueuePolicy {
    pub fn new(max_concurrent_http: usize) -> Self {
        Self {
            max_concurrent_http: max_concurrent_http.max(1),
        }
    }

    /// Aktif HTTP görev sayısı limitin altındaysa bekleyen en eski HTTP görevi döner.
    /// Paused/Failed/Completed/Conflict sayılmaz; Torrent/Media M1'de dispatch edilmez.
    pub fn next_http(&self, tasks: &[Task]) -> Option<TaskId> {
        let active = tasks
            .iter()
            .filter(|t| t.kind == TaskKind::Http && t.is_active())
            .count();
        if active >= self.max_concurrent_http {
            return None;
        }
        tasks
            .iter()
            .filter(|t| t.kind == TaskKind::Http && t.state == TaskState::Queued)
            .min_by_key(|t| (t.created_at, &t.id))
            .map(|t| t.id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn task(kind: TaskKind, state: TaskState, created_at: i64) -> Task {
        let mut t = match kind {
            TaskKind::Http => Task::new_http("https://e.com/f".into(), PathBuf::from("."), None),
            _ => Task::new_torrent("magnet:?xt=urn:btih:aa".into(), PathBuf::from(".")),
        };
        t.state = state;
        t.created_at = created_at;
        t
    }

    #[test]
    fn picks_oldest_queued_http() {
        let p = QueuePolicy::new(3);
        let a = task(TaskKind::Http, TaskState::Queued, 100);
        let b = task(TaskKind::Http, TaskState::Queued, 50);
        assert_eq!(p.next_http(&[a.clone(), b.clone()]), Some(b.id));
    }

    #[test]
    fn respects_max_concurrent() {
        let p = QueuePolicy::new(1);
        let active = task(TaskKind::Http, TaskState::Downloading, 10);
        let waiting = task(TaskKind::Http, TaskState::Queued, 20);
        assert_eq!(p.next_http(&[active, waiting]), None);
    }

    #[test]
    fn frees_slot_when_done() {
        let p = QueuePolicy::new(1);
        let done = task(TaskKind::Http, TaskState::Completed, 10);
        let waiting = task(TaskKind::Http, TaskState::Queued, 20);
        assert_eq!(p.next_http(&[done, waiting.clone()]), Some(waiting.id));
    }

    #[test]
    fn non_active_states_dont_count() {
        let p = QueuePolicy::new(1);
        let paused = task(TaskKind::Http, TaskState::Paused, 10);
        let failed = task(TaskKind::Http, TaskState::Failed, 11);
        let waiting = task(TaskKind::Http, TaskState::Queued, 20);
        assert_eq!(
            p.next_http(&[paused, failed, waiting.clone()]),
            Some(waiting.id)
        );
    }

    #[test]
    fn torrent_not_dispatched_in_m1() {
        let p = QueuePolicy::new(3);
        let m = task(TaskKind::Torrent, TaskState::Queued, 10);
        assert_eq!(p.next_http(&[m]), None);
    }

    #[test]
    fn zero_limit_clamped_to_one() {
        let p = QueuePolicy::new(0);
        assert_eq!(p.max_concurrent_http, 1);
    }
}
