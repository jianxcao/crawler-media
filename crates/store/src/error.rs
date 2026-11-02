//! Store operation and layout errors.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("writer lock order violation: held {held:?}, requested {requested:?}")]
    LockOrder {
        held: Vec<crate::WriterRank>,
        requested: crate::WriterRank,
    },
    #[error("reader pool is temporarily busy")]
    ReadBusy,
    #[error("reader pool is closed")]
    ReadClosed,
    #[error("reader deadline expired")]
    ReadDeadline,
    #[error("reader operation cancelled")]
    ReadCancelled,
    #[error("reader lease belongs to another database or Store")]
    ReadLeaseMismatch,
    #[error("reader operation reentered its active database connection")]
    ReadReentrant,
    #[error("database writer is temporarily busy")]
    WriteBusy,
    #[error("library {library_id} query projection is temporarily unavailable")]
    QueryUnavailable { library_id: domain::LibraryId },
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid media kind: {0}")]
    MediaKind(String),
    #[error("invalid fetch mode: {0}")]
    FetchMode(String),
    #[error("invalid uuid: {0}")]
    Uuid(#[from] uuid::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("missing {0}")]
    Missing(String),
    #[error("invalid argument: {0}")]
    Invalid(String),
    /// A database file exists but was written by another layout. The layout is
    /// not migrated (see `docs/superpowers/specs/2026-10-18-library-storage-redesign.md`):
    /// the file is refused and the operator rebuilds it.
    #[error("layout mismatch: {0}")]
    LayoutMismatch(String),
    #[error("protected: {0}")]
    Protected(String),
    #[error("credential conflict: {0}")]
    CredentialConflict(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("subscription coverage overlaps {subscribe_id}")]
    SubscribeOverlap { subscribe_id: domain::SubscribeId },
    #[error("password hashing error: {0}")]
    PasswordHash(String),
}

impl StoreError {
    /// The connection's on-disk layout is not the one this build expects, so
    /// nothing may be read or written through it.
    ///
    /// `Invalid` still carries refusals for the app file, while the library and
    /// subscribe files report the same condition as `LayoutMismatch`; callers that
    /// react to a refusal (the read pool closes on it) must match both, so they ask
    /// here instead of repeating the variant list at each site.
    pub fn is_layout_refusal(&self) -> bool {
        matches!(self, Self::Invalid(_) | Self::LayoutMismatch(_))
    }
}
