use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaFieldError {
    pub path: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PackageError {
    #[error("package exceeds size or entry limit: {0}")]
    LimitExceeded(String),
    #[error("compression ratio exceeds limit: {0}")]
    CompressionRatioExceeded(String),
    #[error("invalid archive structure: {0}")]
    InvalidArchive(String),
    #[error("unsafe archive entry path `{path}`: {reason}")]
    UnsafePath { path: String, reason: String },
    #[error("forbidden archive entry type `{path}`: {reason}")]
    ForbiddenEntryType { path: String, reason: String },
    #[error("duplicate archive path `{0}`")]
    DuplicatePath(String),
    #[error("path collision between `{path}` and `{existing}`")]
    PathCollision { path: String, existing: String },
    #[error("encrypted archive entry `{0}` is not supported")]
    EncryptedArchiveEntry(String),
    #[error("unsupported compression method `{method}` on `{path}`")]
    UnsupportedCompression { path: String, method: u16 },
    #[error("forbidden or undeclared executable `{0}`")]
    ForbiddenExecutable(String),
    #[error("missing required plugin.toml manifest")]
    MissingManifest,
    #[error("package signature required in normal mode")]
    SignatureRequired,
    #[error("signature verification failed: {0}")]
    InvalidSignature(String),
    #[error("signing key `{0}` is not trusted")]
    UntrustedKey(String),
    #[error("signing key `{0}` has been revoked")]
    KeyRevoked(String),
    #[error("invalid plugin manifest: {0}")]
    InvalidManifest(String),
    #[error("empty plugin package: must declare at least one component, site profile, or ui")]
    EmptyPackage,
    #[error("unsupported plugin API `{api}` or world `{world}`")]
    UnsupportedWorld { api: String, world: String },
    #[error("unsupported hook `{0}`")]
    UnsupportedHook(String),
    #[error("unknown or disallowed capability `{0}`")]
    UnknownCapability(String),
    #[error("invalid config schema: {0}")]
    InvalidSchema(String),
    #[error("config validation failed")]
    InvalidConfig { fields: Vec<SchemaFieldError> },
    #[error("invalid UI asset manifest: {0}")]
    InvalidUiManifest(String),
    #[error("forbidden UI asset `{0}`")]
    ForbiddenUiAsset(String),
    #[error("UI asset limit exceeded: {0}")]
    UiAssetLimitExceeded(String),
}
