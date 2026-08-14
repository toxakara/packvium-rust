use thiserror::Error;

pub type PackResult<T> = Result<T, PackError>;

#[derive(Debug, Error)]
pub enum PackError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("unsupported_feature: {0}")]
    UnsupportedFeature(String),
    #[error("unsupported unit: {0}")]
    UnsupportedUnit(String),
    #[error("invalid numeric value: {0}")]
    InvalidNumber(String),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("solver result failed independent validation: {0}")]
    InvalidSolution(String),
    #[error("time limit reached")]
    TimeLimit,
}
