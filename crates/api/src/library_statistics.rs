use axum::http::StatusCode;
use axum::response::Response;
use domain::UserId;
use std::collections::HashMap;
use store::Library;

use crate::ApiState;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LibraryStatistics {
    pub item_count: u64,
    pub file_count: u64,
    pub total_size_bytes: u64,
    pub strm_file_count: u64,
    pub refreshed_at_ms: i64,
    pub stale: bool,
}

pub(crate) fn statistics_for(
    store: &store::Store,
    library: &Library,
) -> Result<LibraryStatistics, store::StoreError> {
    let stats = store.library_roster_stats(&library.id)?;
    Ok(LibraryStatistics {
        item_count: stats.item_count,
        file_count: stats.file_count,
        total_size_bytes: stats.total_size_bytes,
        strm_file_count: stats.strm_file_count,
        refreshed_at_ms: 0,
        stale: false,
    })
}
pub(crate) async fn collect_statistics_for_roster(
    state: ApiState,
    _user_id: Option<UserId>,
    roster: &[Library],
    _is_admin: bool,
) -> Result<HashMap<String, LibraryStatistics>, Response> {
    let roster = roster.to_vec();
    crate::http::store_wait::store_read(&state, move |store| {
        roster
            .into_iter()
            .map(|library| {
                statistics_for(store, &library)
                    .map(|stats| (library.id, stats))
                    .map_err(|error| {
                        tracing::error!(%error, "Library roster statistics failed");
                        crate::http::err(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "store.error",
                            &error.to_string(),
                        )
                    })
            })
            .collect()
    })
    .await
    .map_err(|error| error.response())?
}
