use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum NetGuardError {
    #[error("invalid target url: {0}")]
    InvalidUrl(String),
    #[error("blocked target address: {0}")]
    BlockedAddress(String),
    #[error("https to http downgrade redirect blocked")]
    HttpsDowngradeBlocked,
    #[error("too many redirects (max {0})")]
    TooManyRedirects(u8),
    #[error("invalid redirect response: {0}")]
    InvalidRedirect(String),
    #[error("response body exceeds limit of {limit} bytes")]
    BodyTooLarge { limit: u64 },
    #[error("request body exceeds limit of {limit} bytes")]
    RequestTooLarge { limit: u64 },
    #[error("unsupported outbound route: {0}")]
    UnsupportedRoute(String),
    #[error("forbidden caller header: {0}")]
    HeaderForbidden(String),
    #[error("http method not allowed: {0}")]
    MethodNotAllowed(String),
    #[error("outbound request timed out: {0}")]
    Timeout(String),
    #[error("outbound request outcome unknown after transmission: {0}")]
    PostSendUnknown(String),
    #[error("outbound request outcome unknown after transmission; follow-up blocked: {0}")]
    PostSendBlocked(#[source] Box<NetGuardError>),
    #[error("outbound transport error: {0}")]
    Transport(String),
}

impl NetGuardError {
    pub fn is_post_send_unknown(&self) -> bool {
        matches!(self, Self::PostSendUnknown(_) | Self::PostSendBlocked(_))
    }

    pub fn cause(&self) -> &Self {
        let mut cur = self;
        while let Self::PostSendBlocked(inner) = cur {
            cur = inner;
        }
        cur
    }
}
