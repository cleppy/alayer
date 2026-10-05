//! Sidecar köprüleri. Core bu crate'i tanımaz.

mod aria2;
mod formats;
mod progress;
mod ytdlp;

pub use aria2::Aria2Engine;
pub use formats::{curate, title_of, FormatChoice};
pub use ytdlp::{probe_media, YtdlpResolver};
