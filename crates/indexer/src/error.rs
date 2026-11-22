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
