use super::*;

pub(crate) async fn target_prefs(State(state): State<ApiState>) -> Response {
    crate::http::store_wait::store_read(&state, move |store| {
        let raw = store.get_setting("download.target_prefs").ok().flatten();
        let prefs = raw
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or_else(|| json!({}));
        ok(prefs).into_response()
    })
    .await
    .unwrap_or_else(|error| error.response())
}

pub(crate) async fn put_target_pref(
    State(state): State<ApiState>,
    Path(category): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    crate::http::store_wait::store_write(&state, move |store| {
        let raw = store.get_setting("download.target_prefs").ok().flatten();
        let mut prefs = raw
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or_else(|| json!({}));
        if body.is_null() {
            if let Some(obj) = prefs.as_object_mut() {
                obj.remove(&category);
            }
        } else {
            prefs[category] = body;
        }
        if let Err(error) = store.put_setting("download.target_prefs", &prefs.to_string()) {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
        ok(prefs).into_response()
    })
    .await
    .unwrap_or_else(|response| response)
}
