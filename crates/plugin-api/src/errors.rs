#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    PermissionDenied,
    WrongContext,
    NotFound,
    InvalidArgument,
    StaleHandle,
    StaleGeneration,
    Conflict,
    QuotaExceeded,
    Busy,
    DeadlineExceeded,
    Cancelled,
    Unsupported,
    Upstream,
    UnknownOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpstreamKind {
    AuthExpired,
    RateLimited,
    Challenge,
    Network,
    Parse,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorDetail {
    Upstream(UpstreamKind),
    BlockedAddress,
    BodyTooLarge,
    BrowserIsolationUnavailable,
    UnsupportedRoute,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    pub detail: Option<ErrorDetail>,
    pub retry_after_ms: Option<u64>,
    pub operation_id: Option<String>,
}
