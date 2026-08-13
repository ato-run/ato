use thiserror::Error;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum WireError {
    #[error("invalid wire value: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, WireError>;
