use crate::{DownloaderRow, Store, StoreError};
use domain::DownloaderId;

impl Store {
    /// Verify status and error are one result of one exact observed configuration.
    /// An IMMEDIATE transaction prevents another connection changing config between
    /// the comparison and status publication; any second-statement failure rolls back.
    pub fn record_downloader_verification(
        &self,
        expected: &DownloaderRow,
        outcome: Result<(), &str>,
    ) -> Result<bool, StoreError> {
        self.with_app(|app| app.record_downloader_verification(expected, outcome))
    }

    pub fn insert_downloader(&self, row: &DownloaderRow) -> Result<(), StoreError> {
        self.with_app(|app| app.insert_downloader(row))
    }

    pub fn save_downloader(&self, row: &DownloaderRow) -> Result<(), StoreError> {
        self.with_app(|app| app.save_downloader(row))
    }

    /// Flip a downloader's enabled flag. Returns false when the row is absent.
    pub fn set_downloader_enabled(
        &self,
        id: DownloaderId,
        enabled: bool,
    ) -> Result<bool, StoreError> {
        self.with_app(|app| app.set_downloader_enabled(id, enabled))
    }

    pub fn list_downloaders(&self) -> Result<Vec<DownloaderRow>, StoreError> {
        self.with_app_read(|app| app.list_downloaders())
    }

    pub fn get_downloader(&self, id: DownloaderId) -> Result<Option<DownloaderRow>, StoreError> {
        self.with_app_read(|app| app.get_downloader(id))
    }

    pub fn delete_downloader(&self, id: DownloaderId) -> Result<bool, StoreError> {
        self.with_app(|app| app.delete_downloader(id))
    }

    pub fn default_downloader(&self) -> Result<Option<DownloaderRow>, StoreError> {
        self.with_app_read(|app| app.default_downloader())
    }

    pub fn set_default_downloader(&self, id: DownloaderId) -> Result<bool, StoreError> {
        self.with_app(|app| app.set_default_downloader(id))
    }
}
