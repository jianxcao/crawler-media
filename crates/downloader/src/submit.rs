use async_trait::async_trait;
use domain::{DownloaderId, MediaId, PluginOperationId, Release, SubscribeId, Torrent};

use crate::{
    DownloaderError, SubmissionPurpose, TorrentFetchError, TorrentParseError, TorrentSource,
};

#[derive(Clone, Debug)]
pub struct FrozenDownloadPlan {
    pub torrent: Torrent,
    pub source: TorrentSource,
    pub purpose: SubmissionPurpose,
    pub downloader_id: Option<DownloaderId>,
    pub frozen_endpoint: Option<String>,
    pub subscribe_id: Option<SubscribeId>,
    pub subscribe_generation: Option<i64>,
    pub policy_revision: Option<i64>,
    pub media_id: Option<MediaId>,
    pub score: Option<i32>,
    pub release_override: Option<Release>,
    pub force: bool,
    pub save_path: Option<String>,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub paused: bool,
    pub upload_limit: Option<u64>,
    pub download_limit: Option<u64>,
    pub sequential: bool,
    pub first_last_piece: bool,
    pub budget_key: Option<String>,
    pub plan_digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadOpContext {
    pub operation_id: PluginOperationId,
    pub cause_id: String,
    pub subject: String,
    pub idempotency_key: String,
    pub plugin_id: Option<String>,
    pub generation: Option<u64>,
    pub config_revision: Option<u64>,
    pub dry_run: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadOutcome {
    Submitted {
        operation_id: PluginOperationId,
        downloader_id: Option<DownloaderId>,
        info_hash: Option<String>,
        bytes_digest: Option<String>,
        verified_total_bytes: Option<i64>,
        already_owned: bool,
    },
    Rejected {
        operation_id: PluginOperationId,
        reason: String,
        broken_task_hash: Option<String>,
    },
    Deferred {
        operation_id: PluginOperationId,
        until_unix: i64,
        reason: String,
    },
    Unknown {
        operation_id: PluginOperationId,
        downloader_id: Option<DownloaderId>,
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadSubmitError {
    #[error("invalid download plan: {0}")]
    InvalidPlan(String),
    #[error("downloader unavailable: {0}")]
    Unavailable(String),
    #[error("download fetch failed before submission: {0}")]
    Fetch(#[from] TorrentFetchError),
    #[error("torrent metainfo invalid: {0}")]
    InvalidMetainfo(#[from] TorrentParseError),
    #[error("download operation conflict: {0}")]
    Conflict(String),
    #[error("download operation cancelled")]
    Cancelled,
    #[error("quota budget exceeded: {0}")]
    QuotaExceeded(String),
    /// Persistence writer or reader pool was temporarily busy.
    #[error("persistence busy: {0}")]
    Busy(String),
    #[error("persistence error: {0}")]
    Storage(String),
    #[error("downloader pre-send error: {0}")]
    Downloader(#[from] DownloaderError),
}

#[async_trait]
pub trait DownloadSubmitter: Send + Sync {
    async fn submit(
        &self,
        plan: FrozenDownloadPlan,
        op: DownloadOpContext,
    ) -> Result<DownloadOutcome, DownloadSubmitError>;
}
