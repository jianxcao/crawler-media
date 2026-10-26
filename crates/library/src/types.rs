use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferMode {
    Hardlink,
    Copy,
    Move,
}

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("Library work cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("ffprobe failed: {0}")]
    Probe(String),
    #[error("naming template: {0}")]
    Naming(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FileQuality {
    pub resolution: Option<String>,
    pub codec: Option<String>,
    pub hdr: Option<String>,
}
