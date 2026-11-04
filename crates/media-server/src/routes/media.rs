mod images;
mod playback;
mod source;

use axum::Router;
use std::sync::Arc;

use crate::provider::MediaServerProvider;

pub(super) use playback::playback_info;
pub(super) use source::media_source;

pub(super) fn routes(provider: Arc<dyn MediaServerProvider>) -> Router {
    let root = images::routes(provider.clone()).merge(playback::routes(provider));
    Router::new().merge(root.clone()).nest("/emby", root)
}
