#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SiteFailureKind {
    MissingProvider,
    ProviderDisabled,
    UnsupportedCapability,
    StaleGeneration,
    InvalidInput,
    Fetch,
    Parse,
    BrowserSetup,
    BrowserBusy,
    Budget,
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("profile/provider id `{id}` collides (existing: {existing_source}, new: {new_source})")]
    Collision {
        id: String,
        existing_source: String,
        new_source: String,
    },
    #[error("invalid provider namespace `{id}`: {reason}")]
    InvalidNamespace { id: String, reason: String },
    #[error("invalid provider descriptor `{id}`: {reason}")]
    InvalidDescriptor { id: String, reason: String },
    #[error("stale registry generation: expected {expected}, actual {actual}")]
    StaleExpectedGeneration { expected: u64, actual: u64 },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum IndexerError {
    #[error("Browser busy")]
    BrowserBusy,
    #[error("Browser setup: {0}")]
    BrowserSetup(String),
    #[error("unknown outcome: {0}")]
    UnknownOutcome(String),
    #[error("{0}")]
    Fetch(String),
    #[error("parse: {0}")]
    Parse(String),
    #[error("source not found: {0}")]
    SourceNotFound(String),
    #[error("unknown profile: {0}")]
    UnknownProfile(String),
    #[error("provider `{provider_id}` is disabled: {reason}")]
    ProviderDisabled { provider_id: String, reason: String },
    #[error("provider `{provider_id}` does not support capability `{capability}`")]
    UnsupportedCapability {
        provider_id: String,
        capability: String,
    },
    #[error("stale registry generation: expected {expected}, actual {actual}")]
    StaleGeneration { expected: u64, actual: u64 },
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),
}

impl IndexerError {
    /// Returns `true` only when a fetched response definitively indicates the
    /// torrent target does not exist (`404`/`410` or explicit success envelope
    /// with empty download URL); rate-limit, auth, quota, gateway HTML, and
    /// configuration failures remain retryable.
    pub fn is_definitive_source_rejection(&self) -> bool {
        match self {
            Self::SourceNotFound(_) => true,
            Self::BrowserBusy
            | Self::BrowserSetup(_)
            | Self::UnknownOutcome(_)
            | Self::Fetch(_)
            | Self::Parse(_)
            | Self::UnknownProfile(_)
            | Self::ProviderDisabled { .. }
            | Self::UnsupportedCapability { .. }
            | Self::StaleGeneration { .. }
            | Self::InvalidInput(_)
            | Self::Registry(_)
            | Self::Io(_)
            | Self::Yaml(_) => false,
        }
    }

    pub fn failure_kind(&self) -> SiteFailureKind {
        match self {
            Self::UnknownProfile(_) | Self::Registry(_) => SiteFailureKind::MissingProvider,
            Self::ProviderDisabled { .. } => SiteFailureKind::ProviderDisabled,
            Self::UnsupportedCapability { .. } => SiteFailureKind::UnsupportedCapability,
            Self::StaleGeneration { .. } => SiteFailureKind::StaleGeneration,
            Self::InvalidInput(_) => SiteFailureKind::InvalidInput,
            Self::Fetch(_) | Self::SourceNotFound(_) | Self::Io(_) | Self::UnknownOutcome(_) => {
                SiteFailureKind::Fetch
            }
            Self::Parse(_) | Self::Yaml(_) => SiteFailureKind::Parse,
            Self::BrowserSetup(_) => SiteFailureKind::BrowserSetup,
            Self::BrowserBusy => SiteFailureKind::BrowserBusy,
        }
    }
}
