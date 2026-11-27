use crate::bindings::crawler::plugin::types as wit;
use plugin_api as api;

macro_rules! convert_enum {
    ($name:ident, $($variant:ident),+ $(,)?) => {
        impl From<wit::$name> for api::$name {
            fn from(value: wit::$name) -> Self {
                match value { $(wit::$name::$variant => Self::$variant),+ }
            }
        }
        impl From<api::$name> for wit::$name {
            fn from(value: api::$name) -> Self {
                match value { $(api::$name::$variant => Self::$variant),+ }
            }
        }
    };
}

convert_enum!(
    ErrorCode,
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
    UnknownOutcome
);
convert_enum!(
    UpstreamKind,
    AuthExpired,
    RateLimited,
    Challenge,
    Network,
    Parse
);

impl From<wit::ErrorDetail> for api::ErrorDetail {
    fn from(value: wit::ErrorDetail) -> Self {
        match value {
            wit::ErrorDetail::Upstream(value) => Self::Upstream(value.into()),
            wit::ErrorDetail::BlockedAddress => Self::BlockedAddress,
            wit::ErrorDetail::BodyTooLarge => Self::BodyTooLarge,
            wit::ErrorDetail::BrowserIsolationUnavailable => Self::BrowserIsolationUnavailable,
            wit::ErrorDetail::UnsupportedRoute => Self::UnsupportedRoute,
        }
    }
}
impl From<api::ErrorDetail> for wit::ErrorDetail {
    fn from(value: api::ErrorDetail) -> Self {
        match value {
            api::ErrorDetail::Upstream(value) => Self::Upstream(value.into()),
            api::ErrorDetail::BlockedAddress => Self::BlockedAddress,
            api::ErrorDetail::BodyTooLarge => Self::BodyTooLarge,
            api::ErrorDetail::BrowserIsolationUnavailable => Self::BrowserIsolationUnavailable,
            api::ErrorDetail::UnsupportedRoute => Self::UnsupportedRoute,
        }
    }
}
impl From<wit::ApiError> for api::ApiError {
    fn from(value: wit::ApiError) -> Self {
        Self {
            code: value.code.into(),
            message: value.message,
            detail: value.detail.map(Into::into),
            retry_after_ms: value.retry_after_ms,
            operation_id: value.operation_id,
        }
    }
}
impl From<api::ApiError> for wit::ApiError {
    fn from(value: api::ApiError) -> Self {
        Self {
            code: value.code.into(),
            message: value.message,
            detail: value.detail.map(Into::into),
            retry_after_ms: value.retry_after_ms,
            operation_id: value.operation_id,
        }
    }
}
