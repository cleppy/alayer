//! Sidecar köprüleri. Core bu crate'i tanımaz.

mod aria2;
mod progress;
mod ytdlp;

pub use aria2::Aria2Engine;
pub use ytdlp::YtdlpResolver;
