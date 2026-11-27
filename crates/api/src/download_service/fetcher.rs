use downloader::{
    PreparedTorrent, SubmissionPurpose, TorrentFetchError, TorrentSource, prepare_download_payload,
};
use net_guard::{NetGuardClient, TargetPolicy};

pub trait TorrentPayloadFetcher: Send + Sync {
    fn prepare(
        &self,
        source: &TorrentSource,
        purpose: SubmissionPurpose,
        policy: &TargetPolicy,
    ) -> Result<PreparedTorrent, TorrentFetchError>;
}

pub(crate) struct NetGuardTorrentFetcher {
    client: NetGuardClient,
}

impl Default for NetGuardTorrentFetcher {
    fn default() -> Self {
        Self {
            client: NetGuardClient::new(),
        }
    }
}

impl TorrentPayloadFetcher for NetGuardTorrentFetcher {
    fn prepare(
        &self,
        source: &TorrentSource,
        purpose: SubmissionPurpose,
        policy: &TargetPolicy,
    ) -> Result<PreparedTorrent, TorrentFetchError> {
        prepare_download_payload(&self.client, source, purpose, policy)
    }
}
