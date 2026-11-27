//! Downloader operations and failures.
use crate::{TaskSnapshot, magnet_info_hash};
use domain::Torrent;
use std::{path::PathBuf, sync::Arc};

#[derive(Debug, thiserror::Error)]
pub enum DownloaderError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Message(String),
    /// G-M2: an owned task for this enclosure exists but is unusable
    /// (`error` / `missingFiles`); callers must switch source.
    #[error("existing downloader task {0} is broken")]
    ExistingTaskBroken(String),
    /// Fetching or validating the `.torrent` from the source definitively failed
    /// (HTTP 404/410 or non-`.torrent` body such as an HTML login page); callers
    /// persist this source as rejected and fall back to another candidate.
    /// Transient fetch errors (DNS, timeout, 5xx, 429) and qBittorrent `Fails.`
    /// replies use [`DownloaderError::Message`] so the source can be retried.
    #[error("torrent source rejected: {0}")]
    SourceRejected(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmitPayload {
    TorrentBytes {
        bytes: Vec<u8>,
        bytes_digest: String,
        info_hash: String,
    },
    Magnet {
        uri: String,
        info_hash: String,
    },
}

impl SubmitPayload {
    pub fn info_hash(&self) -> &str {
        match self {
            Self::TorrentBytes { info_hash, .. } | Self::Magnet { info_hash, .. } => info_hash,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubmitOptions {
    pub ownership_tag: String,
    pub save_path: Option<String>,
    pub category: Option<String>,
    pub extra_tags: Vec<String>,
    pub paused: bool,
    pub upload_limit: Option<u64>,
    pub download_limit: Option<u64>,
    pub sequential: bool,
    pub first_last_piece: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientSubmitResult {
    Added {
        already_owned: bool,
        info_hash: String,
    },
    ForeignDuplicate {
        info_hash: String,
    },
    ExistingBroken {
        info_hash: String,
    },
    UnknownOutcome {
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskSpaceStatus {
    Available,
    Unavailable,
    Unknown,
}

impl DiskSpaceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Unavailable => "unavailable",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskSpaceInfo {
    pub available_bytes: Option<u64>,
    pub root_path: Option<PathBuf>,
    pub status: DiskSpaceStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskFileSet {
    pub save_path: String,
    pub files: Vec<PathBuf>,
    pub total_bytes: u64,
    pub claimed_by_app: bool,
    pub tags: Vec<String>,
    pub category: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientTaskQuery {
    pub exists: bool,
    pub info_hash: Option<String>,
    pub owned_by_tag: bool,
    pub broken: bool,
    pub state: Option<String>,
    pub paused: bool,
}

pub trait Downloader: Send + Sync {
    fn submit_verified(
        &self,
        payload: &SubmitPayload,
        options: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError>;

    fn query_task_status(
        &self,
        info_hash: &str,
        ownership_tag: &str,
    ) -> Result<ClientTaskQuery, DownloaderError> {
        let _ = (info_hash, ownership_tag);
        Err(DownloaderError::Message(
            "query_task_status unsupported".into(),
        ))
    }

    fn supports_labels(&self) -> bool {
        true
    }

    /// D-M3: re-teach a client the info hash persisted for `torrent` (e.g.
    /// after a restart). Clients that identify tasks by tags ignore it.
    fn remember_identity(&self, _torrent: &Torrent, _hash: &str) -> Result<(), DownloaderError> {
        Ok(())
    }
    fn completed_files(&self, torrent: &Torrent) -> Result<Vec<PathBuf>, DownloaderError>;

    /// Review H-3: whether the task for `torrent` is a claimed user task.
    fn is_claimed(&self, _torrent: &Torrent) -> Result<bool, DownloaderError> {
        Ok(false)
    }

    /// D-L1: files finished so far (even while other files of the task are
    /// still downloading) and whether the task's wanted files are all done.
    /// The default keeps the whole-task behaviour of `completed_files`.
    fn collectable_files(
        &self,
        torrent: &Torrent,
    ) -> Result<(Vec<PathBuf>, bool), DownloaderError> {
        let files = self.completed_files(torrent)?;
        let done = !files.is_empty();
        Ok((files, done))
    }

    /// Uploaded bytes for torrents in a category (boost/刷流 stats).
    /// Downloaders without upload telemetry return 0.
    fn uploaded_by_category(&self, _category: &str) -> Result<u64, DownloaderError> {
        Ok(0)
    }

    /// 从客户端删除该种子的下载任务（季清理 / 退订联动）。
    /// `delete_files: false` 保留做种文件——Library 侧清理由调用方负责
    /// （wash-cut 语义：删除旧 Library 文件不影响下载器继续做种）。
    /// 默认不支持；qBittorrent / Transmission 已实现。
    fn remove(&self, _torrent: &Torrent, _delete_files: bool) -> Result<(), DownloaderError> {
        Err(DownloaderError::Message("remove not supported".into()))
    }

    /// Destructive Subscribe cleanup must prove exact identity. Never falls back
    /// to `remove`, whose implementations may match names and sizes.
    fn remove_owned(&self, _torrent: &Torrent, _delete_files: bool) -> Result<(), DownloaderError> {
        Err(DownloaderError::Message(
            "exact owned removal unsupported; refusing unsafe cleanup".into(),
        ))
    }

    /// F-1: mark files for which `keep(name)` is false as not-to-download.
    /// Returns how many files were skipped, or `None` when the task or its
    /// file list (magnet metadata) is not known yet, or the client cannot
    /// select files.
    fn select_files(
        &self,
        _torrent: &Torrent,
        _keep: &dyn Fn(&str) -> bool,
    ) -> Result<Option<usize>, DownloaderError> {
        Ok(None)
    }

    /// Canonical actual task identity, not a title-derived snapshot match.
    /// None means ownership cannot be proven (an HTTP delivery has no info hash).
    fn owned_identity(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        Ok(magnet_info_hash(&torrent.enclosure))
    }

    /// The endpoint this connected client actually talks to, as
    /// `"<kind>|<normalized url>"`. Ownership routing compares this instead of
    /// re-deriving configuration, so a runtime override (including secrets
    /// resolved from files) is reflected exactly. None means "cannot report".
    fn endpoint(&self) -> Option<String> {
        None
    }

    /// If this value is a runtime proxy, return the concrete client selected
    /// right now. Cleanup freezes that snapshot so a later configuration
    /// change cannot retarget a planned delete. Concrete clients return None.
    fn snapshot_client(&self) -> Result<Option<Arc<dyn Downloader>>, DownloaderError> {
        Ok(None)
    }

    /// Delete a task by info hash. `delete_files`: remove downloaded data too.
    fn delete_task(&self, _info_hash: &str, _delete_files: bool) -> Result<(), DownloaderError> {
        Err(DownloaderError::Message("delete_task not supported".into()))
    }

    /// Pause a task by info hash.
    fn pause_task(&self, _info_hash: &str) -> Result<(), DownloaderError> {
        Err(DownloaderError::Message("pause_task not supported".into()))
    }

    /// Resume a task by info hash.
    fn resume_task(&self, _info_hash: &str) -> Result<(), DownloaderError> {
        Err(DownloaderError::Message("resume_task not supported".into()))
    }

    /// Get global speed limits. Returns (download_limit_bytes_per_sec, upload_limit_bytes_per_sec).
    /// 0 = unlimited.
    fn get_speed_limits(&self) -> Result<(u64, u64), DownloaderError> {
        Ok((0, 0))
    }

    /// Set global speed limits. 0 = unlimited.
    fn set_speed_limits(
        &self,
        _download_limit: u64,
        _upload_limit: u64,
    ) -> Result<(), DownloaderError> {
        Err(DownloaderError::Message(
            "set_speed_limits not supported".into(),
        ))
    }

    /// Live snapshot of every task the client knows about.
    ///
    /// Used by the task center to report real progress and lifecycle state for
    /// subscription-delivered tasks (which have no info hash in our store).
    /// Clients that cannot enumerate return Err; callers must not confuse
    /// unsupported snapshotting with an empty client.
    fn task_snapshots(&self) -> Result<Vec<TaskSnapshot>, DownloaderError> {
        Err(DownloaderError::Message(
            "task snapshots unsupported".into(),
        ))
    }

    fn task_files_by_hash(&self, _info_hash: &str) -> Result<Option<TaskFileSet>, DownloaderError> {
        Ok(None)
    }

    fn set_task_tags(&self, _info_hash: &str, _tags: &[String]) -> Result<(), DownloaderError> {
        Err(DownloaderError::Message(
            "set_task_tags not supported".into(),
        ))
    }

    fn set_task_category(&self, _info_hash: &str, _category: &str) -> Result<(), DownloaderError> {
        Err(DownloaderError::Message(
            "set_task_category not supported".into(),
        ))
    }

    fn set_task_limits(
        &self,
        _info_hash: &str,
        _upload_limit: Option<u64>,
        _download_limit: Option<u64>,
    ) -> Result<(), DownloaderError> {
        Err(DownloaderError::Message(
            "set_task_limits not supported".into(),
        ))
    }

    fn disk_free_space(&self) -> Result<DiskSpaceInfo, DownloaderError> {
        Ok(DiskSpaceInfo {
            available_bytes: None,
            root_path: None,
            status: DiskSpaceStatus::Unknown,
        })
    }
}
