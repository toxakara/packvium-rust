use std::fmt;
use thiserror::Error;

pub type PackResult<T> = Result<T, PackError>;

// `#[non_exhaustive]` arrived with `InvalidRequest`: adding that variant already broke every
// exhaustive match downstream, so the break is taken once, here, and the next variant is not
// another one.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PackError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("{0}")]
    InvalidRequest(RequestError),
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

impl PackError {
    /// The closed, machine-readable code a caller branches on instead of parsing the message.
    ///
    /// The match is exhaustive on purpose: a new variant does not compile until it names its
    /// code, so a transport that maps codes (the hosted API's HTTP statuses) cannot drift.
    pub fn code(&self) -> &str {
        match self {
            Self::InvalidInput(_) | Self::Serialization(_) => "invalid_input",
            Self::InvalidRequest(error) => error.code(),
            Self::UnsupportedFeature(_) => "unsupported_feature",
            Self::UnsupportedUnit(_) => "unsupported_unit",
            Self::InvalidNumber(_) => "invalid_number",
            Self::InvalidSolution(_) => "solution_failed_validation",
            Self::TimeLimit => "time_limit",
        }
    }

    /// The named refusal behind `invalid_request` / `invalid_fixed_placement`, if this is one.
    pub fn request_error(&self) -> Option<&RequestError> {
        match self {
            Self::InvalidRequest(error) => Some(error),
            _ => None,
        }
    }
}

impl From<RequestError> for PackError {
    fn from(error: RequestError) -> Self {
        Self::InvalidRequest(error)
    }
}

/// A request no engine may answer, named so a caller can branch on it without parsing prose.
///
/// `code` is `invalid_request`, or `invalid_fixed_placement` for a fixed placement that is
/// malformed or cannot hold; `reason` is one of a closed set shared by all four engines;
/// `field` is the RFC 6901 JSON Pointer of the offending value (`""` for the whole request);
/// `detail` is the reason's fixed text. Nothing was solved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestError {
    code: &'static str,
    reason: &'static str,
    field: String,
    detail: String,
}

impl RequestError {
    pub(crate) const INVALID_REQUEST: &'static str = "invalid_request";
    pub(crate) const INVALID_FIXED_PLACEMENT: &'static str = "invalid_fixed_placement";

    pub(crate) fn new(
        reason: &'static str,
        field: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code: Self::INVALID_REQUEST,
            reason,
            field: field.into(),
            detail: detail.into(),
        }
    }

    pub(crate) fn fixed_placement(
        reason: &'static str,
        field: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code: Self::INVALID_FIXED_PLACEMENT,
            reason,
            field: field.into(),
            detail: detail.into(),
        }
    }

    pub fn code(&self) -> &str {
        self.code
    }

    pub fn reason(&self) -> &str {
        self.reason
    }

    pub fn field(&self) -> &str {
        &self.field
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for RequestError {
    /// `code: field: detail`, or `code: detail` for the whole request. A fixed-placement
    /// refusal keeps the message it had before it carried a field, which the plan-revision
    /// conformance run pins across all four engines.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.field.is_empty() || self.code == Self::INVALID_FIXED_PLACEMENT {
            write!(formatter, "{}: {}", self.code, self.detail)
        } else {
            write!(formatter, "{}: {}: {}", self.code, self.field, self.detail)
        }
    }
}

impl std::error::Error for RequestError {}
