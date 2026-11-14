use super::*;

pub(crate) async fn season_cleanup(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
    Json(body): Json<SeasonCleanupInput>,
) -> Response {
    let captured = crate::http::store_wait::store_write(&state, move |store| {
        let (subscribe, _media) =
            crate::http::subscriptions::resolve_subscribe_on(store, &id, user_id)?;
        if (body.delete_library_files || body.delete_torrents) && !user_is_admin(store, user_id) {
            return Err(err(
                StatusCode::FORBIDDEN,
                "subscription.cleanup_forbidden",
                "只有管理员可以联动清理下载任务或媒体库文件",
            ));
        }
        let rows: Vec<_> = if body.delete_library_files {
            store
                .file_coverage_for_media(subscribe.media_id)
                .map_err(store_error)?
                .into_iter()
                .filter(|coverage| {
                    coverage.units.iter().any(|unit| {
                        !unit.is_whole() && body.seasons.contains(&(unit.season as u32))
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        let pending = if body.delete_torrents {
            let mut pending = store.load_pending(subscribe.id).map_err(store_error)?;
            pending.extend(
                store
                    .load_pending_state(subscribe.id, "imported")
                    .map_err(store_error)?,
            );
            pending
        } else {
            Vec::new()
        };
        Ok((subscribe, rows, pending, body))
    })
    .await;
    let (subscribe, rows, pending, body) = match captured {
        Ok(Ok(captured)) => captured,
        Ok(Err(response)) | Err(response) => return response,
    };
    if body.seasons.is_empty() {
        return ok(json!({"queued":true, "binned":0, "pending_cleared":0})).into_response();
    }
    let (binned, file_errors) = crate::http::file_delete::remove_rows(&state, rows).await;
    if !file_errors.is_empty() {
        return err(
            StatusCode::CONFLICT,
            "subscription.file_cleanup_failed",
            &file_errors.join("；"),
        );
    }

    let subscribe_id = subscribe.id;
    let selected: Vec<_> = pending
        .into_iter()
        .filter_map(|(_, pending)| {
            let season = pending
                .release_override
                .as_ref()
                .and_then(|release| release.season)
                .or_else(|| release::parse(&pending.torrent.title).season);
            season
                .is_some_and(|season| body.seasons.contains(&season))
                .then_some(crate::http::subscriptions::TorrentRemovalTarget {
                    subscribe_id,
                    torrent: pending.torrent,
                    downloader_id: pending.downloader_id,
                })
        })
        .collect();
    let (removed, pending_cleared, torrent_errors) =
        remove_torrents_from_client(&state, selected).await;
    if !torrent_errors.is_empty() {
        return err(
            StatusCode::BAD_GATEWAY,
            "subscription.downloader_cleanup_failed",
            &torrent_errors.join("；"),
        );
    }
    ok(json!({
        "queued": true,
        "binned": binned,
        "file_errors": [],
        "pending_cleared": pending_cleared,
        "removed_from_client": removed.len(),
    }))
    .into_response()
}

fn store_error(error: crate::store::StoreError) -> Response {
    tracing::error!(%error, "Subscribe cleanup preparation failed");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}
