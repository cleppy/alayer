use layer_core::Task;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TaskView {
    pub id: String,
    pub kind: String,
    pub state: String,
    pub url: String,
    pub file_name: String,
    pub dest_dir: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub failure: Option<String>,
    pub unresumable: bool,
}

fn tag<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".into())
}

pub fn to_view(task: &Task) -> TaskView {
    TaskView {
        id: task.id.clone(),
        kind: tag(&task.kind),
        state: tag(&task.state),
        url: task.url.clone(),
        file_name: task.file_name.clone(),
        dest_dir: task.dest_dir.to_string_lossy().to_string(),
        downloaded_bytes: task.downloaded_bytes,
        total_bytes: task.total_bytes,
        failure: task.failure.as_ref().map(|f| f.display()),
        unresumable: task.unresumable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use layer_engine_api::{FailureReason, TaskState};
    use std::path::PathBuf;

    #[test]
    fn maps_task_to_view() {
        let mut task = Task::new_http(
            "https://example.com/a.bin".into(),
            PathBuf::from("C:/dl"),
            None,
        );
        task.downloaded_bytes = 12;
        task.total_bytes = Some(40);
        task.unresumable = true;
        task.state = TaskState::Failed;
        task.failure = Some(FailureReason::Network("reset".into()));
        let view = to_view(&task);
        assert_eq!(view.kind, "http");
        assert_eq!(view.state, "failed");
        assert_eq!(view.file_name, "a.bin");
        assert_eq!(view.failure.as_deref(), Some("network error: reset"));
        assert!(view.unresumable);
        assert_eq!(view.downloaded_bytes, 12);
    }
}
