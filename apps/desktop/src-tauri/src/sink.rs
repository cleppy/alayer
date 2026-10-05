use layer_engine_api::{CoreEvent, EventSink};
use serde::Serialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

const PROGRESS_GAP: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiEvent {
    Added {
        task_id: String,
        kind: String,
    },
    State {
        task_id: String,
        state: String,
        failure: Option<String>,
    },
    Progress {
        task_id: String,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        speed_bps: f64,
    },
    Removed {
        task_id: String,
    },
    Updated {
        task_id: String,
    },
}

pub struct TauriSink {
    app: AppHandle,
    last_progress: Mutex<Instant>,
}

impl TauriSink {
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            last_progress: Mutex::new(Instant::now() - PROGRESS_GAP),
        }
    }

    fn send(&self, event: UiEvent) {
        if let Err(e) = self.app.emit("layer", &event) {
            eprintln!("event: {e}");
        }
    }
}

impl EventSink for TauriSink {
    fn emit(&self, event: CoreEvent) {
        match event {
            CoreEvent::TaskAdded { task_id, kind, .. } => {
                let kind = serde_json::to_value(kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| "http".into());
                self.send(UiEvent::Added { task_id, kind });
            }
            CoreEvent::TaskStateChanged {
                task_id,
                new,
                failure,
                ..
            } => {
                let state = serde_json::to_value(new)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| "queued".into());
                self.send(UiEvent::State {
                    task_id,
                    state,
                    failure: failure.map(|f| f.display()),
                });
            }
            CoreEvent::Progress { task_id, progress } => {
                let mut last = self.last_progress.lock().unwrap_or_else(|e| e.into_inner());
                if last.elapsed() < PROGRESS_GAP {
                    return;
                }
                *last = Instant::now();
                self.send(UiEvent::Progress {
                    task_id,
                    downloaded_bytes: progress.downloaded_bytes,
                    total_bytes: progress.total_bytes,
                    speed_bps: progress.speed_bps,
                });
            }
            CoreEvent::Conflict { task_id } => self.send(UiEvent::State {
                task_id,
                state: "conflict".into(),
                failure: Some("kaynak değişti, parçalar birleştirilemez".into()),
            }),
            CoreEvent::TaskRemoved { task_id } => self.send(UiEvent::Removed { task_id }),
            CoreEvent::TaskUpdated { task_id } => self.send(UiEvent::Updated { task_id }),
            CoreEvent::IntakePrompt { .. } => {}
        }
    }
}
