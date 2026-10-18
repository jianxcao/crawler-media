use std::collections::HashMap;
use axum::http::StatusCode;
use axum::response::Response;
use domain::UserId;
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

pub(crate) fn statistics_for(store: &store::Store, library: &Library) -> LibraryStatistics {
    let stats = store.library_roster_stats(&library.id).unwrap_or_default();
    LibraryStatistics {
        item_count: stats.item_count,
        file_count: stats.file_count,
        total_size_bytes: stats.total_size_bytes,
        strm_file_count: stats.strm_file_count,
        refreshed_at_ms: 0,
        stale: false,
    }
}

pub(crate) async fn collect_statistics_for_roster(
    state: ApiState,
    _user_id: Option<UserId>,
    roster: &[Library],
    _is_admin: bool,
) -> Result<HashMap<String, LibraryStatistics>, Response> {
    let mut map = HashMap::new();
    let store = state.store.lock();

    for lib in roster {
        let stats = store.library_roster_stats(&lib.id).map_err(|e| {
            crate::http::err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &e.to_string(),
            )
        })?;

        map.insert(
            lib.id.clone(),
            LibraryStatistics {
                item_count: stats.item_count,
                file_count: stats.file_count,
                total_size_bytes: stats.total_size_bytes,
                strm_file_count: stats.strm_file_count,
                refreshed_at_ms: 0,
                stale: false,
            },
        );
    }

    Ok(map)
}
