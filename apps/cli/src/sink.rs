use layer_engine_api::{CoreEvent, EventSink, TaskState};
use std::sync::Mutex;
use tokio::sync::Notify;

/// Stdout. `add` tamamlanmayı `done` ile bekler.
pub struct StdoutSink {
    pub done: Notify,
    pub exit_ok: Mutex<Option<bool>>,
    watch: Mutex<Option<String>>,
}

impl StdoutSink {
    pub fn new() -> Self {
        Self {
            done: Notify::new(),
            exit_ok: Mutex::new(None),
            watch: Mutex::new(None),
        }
    }

    pub fn watch(&self, id: String) {
        *self.watch.lock().unwrap_or_else(|e| e.into_inner()) = Some(id);
    }
}

impl EventSink for StdoutSink {
    fn emit(&self, event: CoreEvent) {
        let watch = self.watch.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let relevant = |id: &str| watch.as_ref().is_some_and(|w| w == id);
        match event {
            CoreEvent::Progress { task_id, progress } if relevant(&task_id) => {
                let total = progress
                    .total_bytes
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "?".into());
                eprint!(
                    "\r{task_id} {}/{total}  {:.0} B/s   ",
                    progress.downloaded_bytes, progress.speed_bps
                );
            }
            CoreEvent::TaskStateChanged {
                task_id,
                new,
                failure,
                ..
            } if relevant(&task_id) => {
                eprintln!("\n{task_id} -> {new:?}");
                match new {
                    TaskState::Completed => {
                        *self.exit_ok.lock().unwrap_or_else(|e| e.into_inner()) = Some(true);
                        self.done.notify_waiters();
                    }
                    TaskState::Failed | TaskState::Conflict => {
                        if let Some(f) = failure {
                            eprintln!("{}", f.display());
                        }
                        *self.exit_ok.lock().unwrap_or_else(|e| e.into_inner()) = Some(false);
                        self.done.notify_waiters();
                    }
                    _ => {}
                }
            }
            CoreEvent::Conflict { task_id } if relevant(&task_id) => {
                eprintln!("\n{task_id} conflict: kaynak değişti");
                *self.exit_ok.lock().unwrap_or_else(|e| e.into_inner()) = Some(false);
                self.done.notify_waiters();
            }
            CoreEvent::TaskAdded {
                task_id,
                kind,
                source,
            } => {
                eprintln!("added {task_id} {kind:?} via {source:?}");
            }
            CoreEvent::TaskRemoved { task_id } => eprintln!("removed {task_id}"),
            _ => {}
        }
    }
}
