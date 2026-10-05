use crate::db::DbError;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("görev bulunamadı: {0}")]
    NotFound(String),
    #[error("geçersiz durum: {0}")]
    InvalidState(String),
    #[error("veritabanı: {0}")]
    Db(#[from] DbError),
    #[error("motor: {0}")]
    Engine(String),
}
