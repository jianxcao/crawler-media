use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("{0}")]
    Usage(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Package(#[from] plugin_package::PackageError),
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    CommandFailed(String),
    #[error("HTTP {status}: {body}")]
    RemoteHttp { status: u16, body: String },
}
