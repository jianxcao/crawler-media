use std::sync::Arc;

use super::ApiState;
use crate::catalog::Catalog;
use crate::media_posters::MediaPosterSource;

impl ApiState {
    pub fn with_catalog(mut self, catalog: Arc<dyn Catalog>) -> Self {
        self.catalog = catalog.clone();
        self.configuration_memory =
            Arc::new(crate::configuration_memory::ConfigurationMemory::default());
        self.media_posters = Arc::new(crate::media_posters::service::MediaPosterService::new(
            self.store.clone(),
            catalog,
        ));
        *self.scan_catalog.lock() = (self.catalog.clone(), self.media_posters.clone());
        self
    }

    pub(crate) fn refresh_scan_catalog(&mut self) {
        let services = self.scan_catalog.lock();
        self.catalog = services.0.clone();
        self.media_posters = services.1.clone();
    }

    pub fn with_media_posters(mut self, media_posters: Arc<dyn MediaPosterSource>) -> Self {
        self.media_posters = media_posters;
        self.scan_catalog.lock().1 = self.media_posters.clone();
        self
    }
}
