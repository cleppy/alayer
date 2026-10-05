//! layer port ve DTO sözleşmesi.
//!
//! Dependency Rule: `core` ve engine adapter'ları yalnız bu crate'e bağımlı olur;
//! birbirlerini tanımazlar. Composition root (CLI / Tauri kabuk) somut implementasyonları
//! `core`'a enjekte eder.

pub mod types;

pub use types::*;
