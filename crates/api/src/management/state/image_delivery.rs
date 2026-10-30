use super::ApiState;

impl ApiState {
    /// Supply the origin transport while retaining shared bounded image delivery.
    pub fn with_image_fetch(mut self, fetch: crate::http::image_proxy::ImageFetch) -> Self {
        self.image_delivery = crate::http::image_proxy::with_fetch(self.store.data_dir(), fetch);
        self
    }
}
