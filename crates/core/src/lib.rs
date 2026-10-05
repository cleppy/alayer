//! layer domain + use-case katmanı.
//!
//! Bu crate dış dünyayı (Tauri, aria2, yt-dlp, HTTP istemcileri) TANIMAZ.
//! Yalnız `layer-engine-api` portları üzerinden konuşur; SQLite tek doğruluk kaynağıdır.

pub mod db;
pub mod task;

pub use db::{Db, DbError};
pub use task::{InvalidTransition, Task};
