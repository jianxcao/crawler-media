use std::collections::HashMap;
use std::sync::Arc;

use domain::DownloaderId;

use crate::{Downloader, DownloaderError};

pub struct DownloaderSet {
    default: DownloaderId,
    members: HashMap<DownloaderId, Arc<dyn Downloader>>,
}

impl DownloaderSet {
    pub fn new(
        default: DownloaderId,
        members: HashMap<DownloaderId, Arc<dyn Downloader>>,
    ) -> Result<Self, DownloaderError> {
        if !members.contains_key(&default) {
            return Err(DownloaderError::Message(
                "default Downloader is not in the set".into(),
            ));
        }
        Ok(Self { default, members })
    }

    pub fn pick(&self, id: Option<DownloaderId>) -> Result<Arc<dyn Downloader>, DownloaderError> {
        let key = id.unwrap_or(self.default);
        self.members
            .get(&key)
            .cloned()
            .ok_or_else(|| DownloaderError::Message(format!("unknown Downloader {key}")))
    }

    pub fn default_id(&self) -> DownloaderId {
        self.default
    }
}
