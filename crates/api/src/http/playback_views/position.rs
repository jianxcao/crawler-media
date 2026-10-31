//! Absolute-position navigation resolves only an ordering boundary, not a page.
use std::collections::HashMap;

use axum::{
    extract::{Query, State},
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::{ApiState, http::ok};

pub(crate) async fn favorite_position(
    State(state): State<ApiState>,
    axum::Extension(user): axum::Extension<domain::UserId>,
    Query(mut raw): Query<HashMap<String, String>>,
) -> Response {
    let start = match raw.remove("start").as_deref().map(str::parse::<usize>) {
        Some(Ok(start)) => start,
        _ => return super::favorite_read::invalid("start 必须是非负整数"),
    };
    if raw.contains_key("after") || raw.contains_key("limit") {
        return super::favorite_read::invalid("位置查询不接受 after 或 limit");
    }
    let mut query = match super::favorite_read::query(&raw, user) {
        Ok(query) => query,
        Err(response) => return response,
    };
    match crate::http::store_wait::store_read(&state, move |store| {
        super::favorite_read::scope(store, &mut query)?;
        store.playback_favorite_position(&query, start)
    })
    .await
    {
        Ok(Ok(after)) => ok(json!({"start": start, "after": after})).into_response(),
        Ok(Err(error)) => super::favorite_read::store_error(error),
        Err(error) => error.response(),
    }
}
