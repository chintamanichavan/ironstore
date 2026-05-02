use thiserror::Error;

#[derive(Error, Debug)]
pub enum VektaError {
    #[error("Key not found: {0}")]
    KeyNotFound(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Corruption: {0}")]
    Corruption(String),

    #[error("Storage full")]
    StorageFull,

    #[error("Network error: {0}")]
    Network(String),

    #[error("Vector dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch { expected: usize, got: usize },
}

pub type Result<T> = std::result::Result<T, VektaError>;
