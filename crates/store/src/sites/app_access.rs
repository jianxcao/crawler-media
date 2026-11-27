use crate::{ManualCredentialWrite, SiteObservation, Store, StoreError};
use domain::{Filter, FilterId, Site, SiteId};

impl Store {
    pub fn insert_site(&self, site: &Site) -> Result<(), StoreError> {
        self.with_app(|app| app.insert_site(site))
    }

    pub fn get_site(&self, id: SiteId) -> Result<Option<Site>, StoreError> {
        self.with_app_read(|app| app.get_site(id))
    }
    pub fn observe_site(&self, id: SiteId) -> Result<Option<SiteObservation>, StoreError> {
        self.with_app_read(|app| app.observe_site(id))
    }

    pub fn list_enabled_sites(&self) -> Result<Vec<Site>, StoreError> {
        self.with_app_read(|app| app.list_enabled_sites())
    }

    pub fn list_sites(&self) -> Result<Vec<Site>, StoreError> {
        self.with_app_read(|app| app.list_sites())
    }

    pub fn set_site_enabled(&self, id: SiteId, enabled: bool) -> Result<bool, StoreError> {
        self.with_app(|app| app.set_site_enabled(id, enabled))
    }

    pub fn delete_site(&self, id: SiteId) -> Result<bool, StoreError> {
        self.with_app(|app| app.delete_site(id))
    }

    /// Publish a cookie only while the captured revision and epoch are current.
    pub fn save_site_if_current(
        &self,
        observed: &SiteObservation,
        cookie: Option<&str>,
    ) -> Result<bool, StoreError> {
        self.with_app(|app| app.save_site_if_current(observed, cookie))
    }

    pub fn save_manual_site_if_current(
        &self,
        site: &Site,
        expected: &SiteObservation,
        intent: ManualCredentialWrite,
    ) -> Result<bool, StoreError> {
        self.with_app(|app| app.save_manual_site_if_current(site, expected, intent))
    }

    pub fn save_site(
        &self,
        site: &Site,
        intent: ManualCredentialWrite,
    ) -> Result<bool, StoreError> {
        self.with_app(|app| app.save_site(site, intent))
    }

    pub fn insert_filter(&self, filter: &Filter) -> Result<(), StoreError> {
        self.with_app(|app| app.insert_filter(filter))
    }

    pub fn save_filter(&self, filter: &Filter) -> Result<(), StoreError> {
        self.with_app(|app| app.save_filter(filter))
    }

    pub fn delete_filter(&self, id: FilterId) -> Result<bool, StoreError> {
        self.with_app(|app| app.delete_filter(id))
    }

    pub fn get_filter(&self, id: FilterId) -> Result<Option<Filter>, StoreError> {
        self.with_app_read(|app| app.get_filter(id))
    }

    pub fn list_filters(&self) -> Result<Vec<Filter>, StoreError> {
        self.with_app_read(|app| app.list_filters())
    }
}
