//! layer domain + use-case katmanı.
//!
//! Bu crate dış dünyayı (Tauri, aria2, yt-dlp, HTTP istemcileri) TANIMAZ.
//! Yalnız `layer-engine-api` portları üzerinden konuşur; SQLite tek doğruluk kaynağıdır.

pub mod db;
pub mod error;
pub mod intake;
pub mod manager;
pub mod queue;
pub mod task;

pub use db::{Db, DbError};
pub use error::CoreError;
pub use intake::classify_url;
pub use manager::TaskManager;
pub use queue::QueuePolicy;
pub use task::{InvalidTransition, Task};
