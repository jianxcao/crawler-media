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
