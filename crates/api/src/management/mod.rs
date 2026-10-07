mod error;
mod filters;
mod state;
mod subscribes;

use axum::Router;

pub(crate) use error::ApiError;
pub(crate) use filters::default_filter_id;
pub(crate) use state::SubscribeDeletionReservation;
pub use state::{ApiState, ApiStateError};
pub(crate) use subscribes::run_subscribe;

pub fn router(state: ApiState) -> Router {
    let media_server_provider = std::sync::Arc::new(
        crate::media_server_provider::ApiServerProvider::new(state.clone()),
    );
    let ms_routes = media_server::routes(media_server_provider.clone());
    let ms_media_routes = media_server::media_routes(media_server_provider);

    let jellyfin = Router::new().merge(ms_routes).merge(ms_media_routes);

    Router::new()
        .nest("/api/v1", crate::http::router(state))
        .merge(jellyfin)
        .layer(crate::ui::cors_layer_from_env())
}
