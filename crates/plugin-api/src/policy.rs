#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub handle: String,
    pub site_id: String,
    pub provider: String,
    pub generation: u64,
    pub title: String,
    pub size: Option<u64>,
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    pub published_at: Option<u64>,
    pub free: Option<bool>,
    pub hr: Option<bool>,
    pub release: Option<String>,
    pub score: Option<i32>,
    pub labels: Vec<String>,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteFailure {
    pub site_id: String,
    pub error: crate::ApiError,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchAfter {
    pub candidates: Vec<Candidate>,
    pub site_failures: Vec<SiteFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    #[error("recursive plugin invocation")]
    Reentrant,
    #[error("unsupported capability")]
    Unsupported,
    #[error("guest API error: {0:?}")]
    Guest(crate::ApiError),
    #[error("runtime trap: {0}")]
    Trap(String),
    #[error("invalid component: {0}")]
    InvalidComponent(String),
    #[error("invalid patch: {0}")]
    InvalidPatch(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("busy")]
    Busy,
    #[error("deadline exceeded")]
    DeadlineExceeded,
    #[error("cancelled")]
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct FilterOutcome {
    pub candidates: Vec<Candidate>,
    pub site_failures: Vec<SiteFailure>,
    pub failure: Option<Failure>,
}
