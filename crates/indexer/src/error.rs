#[derive(Debug, thiserror::Error)]
pub enum IndexerError {
    #[error("Browser busy")]
    BrowserBusy,
    #[error("Browser setup: {0}")]
    BrowserSetup(String),
    #[error("{0}")]
    Fetch(String),
    #[error("parse: {0}")]
    Parse(String),
    #[error("unknown profile: {0}")]
    UnknownProfile(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),
}

impl IndexerError {
    /// Returns `true` only when a fetched response was parsed and definitively
    /// lacks a valid download target; transient, infrastructure, and
    /// configuration failures remain retryable.
    pub fn is_definitive_source_rejection(&self) -> bool {
        match self {
            Self::Parse(_) => true,
            Self::BrowserBusy
            | Self::BrowserSetup(_)
            | Self::Fetch(_)
            | Self::UnknownProfile(_)
            | Self::Io(_)
            | Self::Yaml(_) => false,
        }
    }
}
