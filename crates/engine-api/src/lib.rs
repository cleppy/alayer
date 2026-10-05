//! layer port ve DTO sözleşmesi.
//!
//! Dependency Rule: `core` ve engine adapter'ları yalnız bu crate'e bağımlı olur;
//! birbirlerini tanımazlar. Composition root (CLI / Tauri kabuk) somut implementasyonları
//! `core`'a enjekte eder.

pub mod budget;
pub mod ports;
pub mod types;

pub use budget::*;
pub use ports::*;
pub use types::*;
