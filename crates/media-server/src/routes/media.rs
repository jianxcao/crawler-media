mod images;
mod playback;

use axum::Router;
use std::sync::Arc;

use crate::provider::MediaServerProvider;

pub(super) use playback::playback_info;

pub(super) fn routes(provider: Arc<dyn MediaServerProvider>) -> Router {
    let root = images::routes(provider.clone()).merge(playback::routes(provider));
    Router::new().merge(root.clone()).nest("/emby", root)
}
