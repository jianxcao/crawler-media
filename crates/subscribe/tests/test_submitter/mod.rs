#![allow(dead_code)]

use domain::PluginOperationId;
use downloader::{
    ClientSubmitResult, DownloadOpContext, DownloadOutcome, DownloadSubmitError, Downloader,
    FrozenDownloadPlan, SubmitOptions, SubmitPayload, TorrentSource, magnet_info_hash,
    ownership_tag, parse_verified_torrent,
};
use subscribe::SyncDownloadSubmitter;

pub struct DirectTestSubmitter<'a, D: Downloader + ?Sized>(pub &'a D);

fn synthetic_torrent_bytes(name: &str) -> Vec<u8> {
    let clean: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-')
        .take(40)
        .collect();
    let clean = if clean.is_empty() { "test" } else { &clean };
    let info = format!(
        "d6:lengthi1024e4:name{}:{clean}12:piece lengthi16384e6:pieces20:01234567890123456789e",
        clean.len()
    );
    let mut out = Vec::from(b"d8:announce20:https://tracker/test4:info");
    out.extend_from_slice(info.as_bytes());
    out.push(b'e');
    out
}

impl<D: Downloader + ?Sized> SyncDownloadSubmitter for DirectTestSubmitter<'_, D> {
    fn submit_sync(
        &self,
        plan: FrozenDownloadPlan,
        op: DownloadOpContext,
    ) -> Result<DownloadOutcome, DownloadSubmitError> {
        let payload = match &plan.source {
            TorrentSource::Magnet { uri } => {
                let hash = magnet_info_hash(uri)
                    .ok_or_else(|| DownloadSubmitError::InvalidPlan("invalid magnet".into()))?;
                SubmitPayload::Magnet {
                    uri: uri.clone(),
                    info_hash: hash,
                }
            }
            TorrentSource::Http(_) => {
                let bytes = synthetic_torrent_bytes(&plan.torrent.enclosure);
                let parsed = parse_verified_torrent(&bytes)?;
                SubmitPayload::TorrentBytes {
                    bytes: parsed.bytes,
                    bytes_digest: parsed.bytes_digest,
                    info_hash: parsed.info_hash,
                }
            }
            TorrentSource::PluginRef(r) => {
                return Err(DownloadSubmitError::InvalidPlan(format!(
                    "unresolved plugin download_ref `{r}`"
                )));
            }
        };
        let opts = SubmitOptions {
            ownership_tag: ownership_tag(&plan.torrent.enclosure),
            save_path: plan.save_path,
            category: plan.category,
            extra_tags: plan.tags,
            paused: plan.paused,
            upload_limit: plan.upload_limit,
            download_limit: plan.download_limit,
            sequential: plan.sequential,
            first_last_piece: plan.first_last_piece,
        };
        match self.0.submit_verified(&payload, &opts)? {
            ClientSubmitResult::Added {
                already_owned,
                info_hash,
            } => {
                let _ = self.0.remember_identity(&plan.torrent, &info_hash);
                Ok(DownloadOutcome::Submitted {
                    operation_id: op.operation_id,
                    downloader_id: plan.downloader_id,
                    info_hash: Some(info_hash),
                    bytes_digest: None,
                    verified_total_bytes: Some(1024),
                    already_owned,
                })
            }
            ClientSubmitResult::ExistingBroken { info_hash } => Ok(DownloadOutcome::Rejected {
                operation_id: op.operation_id,
                reason: "existing_broken".into(),
                broken_task_hash: Some(info_hash),
            }),
            ClientSubmitResult::ForeignDuplicate { .. } => Ok(DownloadOutcome::Rejected {
                operation_id: op.operation_id,
                reason: "existing_foreign".into(),
                broken_task_hash: None,
            }),
            ClientSubmitResult::UnknownOutcome { reason } => Ok(DownloadOutcome::Unknown {
                operation_id: op.operation_id,
                downloader_id: plan.downloader_id,
                reason,
            }),
        }
    }
}

pub fn unused_op_id() -> PluginOperationId {
    PluginOperationId::new()
}

pub struct TestMemoryDownloader(pub downloader::MemoryDownloader);

impl TestMemoryDownloader {
    pub fn new(stage: impl AsRef<std::path::Path>) -> Self {
        Self(downloader::MemoryDownloader::new(stage))
    }
}

impl std::ops::Deref for TestMemoryDownloader {
    type Target = downloader::MemoryDownloader;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Downloader for TestMemoryDownloader {
    fn remember_identity(
        &self,
        torrent: &domain::Torrent,
        hash: &str,
    ) -> Result<(), downloader::DownloaderError> {
        self.0.remember_identity(torrent, hash)
    }
    fn select_files(
        &self,
        torrent: &domain::Torrent,
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<Option<usize>, downloader::DownloaderError> {
        self.0.select_files(torrent, keep)
    }
    fn submit_verified(
        &self,
        payload: &SubmitPayload,
        options: &SubmitOptions,
    ) -> Result<ClientSubmitResult, downloader::DownloaderError> {
        self.0.submit_verified(payload, options)
    }
    fn query_task_status(
        &self,
        info_hash: &str,
        ownership_tag: &str,
    ) -> Result<downloader::ClientTaskQuery, downloader::DownloaderError> {
        self.0.query_task_status(info_hash, ownership_tag)
    }
    fn supports_labels(&self) -> bool {
        self.0.supports_labels()
    }
    fn completed_files(
        &self,
        torrent: &domain::Torrent,
    ) -> Result<Vec<std::path::PathBuf>, downloader::DownloaderError> {
        self.0.completed_files(torrent)
    }
    fn is_claimed(&self, torrent: &domain::Torrent) -> Result<bool, downloader::DownloaderError> {
        self.0.is_claimed(torrent)
    }
    fn collectable_files(
        &self,
        torrent: &domain::Torrent,
    ) -> Result<(Vec<std::path::PathBuf>, bool), downloader::DownloaderError> {
        self.0.collectable_files(torrent)
    }
    fn uploaded_by_category(&self, category: &str) -> Result<u64, downloader::DownloaderError> {
        self.0.uploaded_by_category(category)
    }
    fn remove(
        &self,
        torrent: &domain::Torrent,
        delete_files: bool,
    ) -> Result<(), downloader::DownloaderError> {
        self.0.remove(torrent, delete_files)
    }
    fn remove_owned(
        &self,
        torrent: &domain::Torrent,
        delete_files: bool,
    ) -> Result<(), downloader::DownloaderError> {
        self.0.remove_owned(torrent, delete_files)
    }
    fn owned_identity(
        &self,
        torrent: &domain::Torrent,
    ) -> Result<Option<String>, downloader::DownloaderError> {
        self.0.owned_identity(torrent)
    }
    fn endpoint(&self) -> Option<String> {
        self.0.endpoint()
    }
    fn delete_task(
        &self,
        info_hash: &str,
        delete_files: bool,
    ) -> Result<(), downloader::DownloaderError> {
        self.0.delete_task(info_hash, delete_files)
    }
    fn pause_task(&self, info_hash: &str) -> Result<(), downloader::DownloaderError> {
        self.0.pause_task(info_hash)
    }
    fn resume_task(&self, info_hash: &str) -> Result<(), downloader::DownloaderError> {
        self.0.resume_task(info_hash)
    }
    fn task_snapshots(&self) -> Result<Vec<downloader::TaskSnapshot>, downloader::DownloaderError> {
        self.0.task_snapshots()
    }
}

impl SyncDownloadSubmitter for TestMemoryDownloader {
    fn submit_sync(
        &self,
        plan: FrozenDownloadPlan,
        op: DownloadOpContext,
    ) -> Result<DownloadOutcome, DownloadSubmitError> {
        DirectTestSubmitter(&self.0).submit_sync(plan, op)
    }
}
