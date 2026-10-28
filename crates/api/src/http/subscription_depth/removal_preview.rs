use super::*;

pub(crate) async fn removal_preview(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
) -> Response {
    let captured = crate::http::store_wait::store_read(&state, move |store| {
        let (subscribe, media) =
            crate::http::subscriptions::resolve_subscribe_on(store, &id, user_id)?;
        let files = store
            .file_coverage_for_media(subscribe.media_id)
            .map_err(store_error)?;
        let bytes: u64 = files
            .iter()
            .map(|coverage| coverage.file.size_bytes.unwrap_or(0))
            .sum();
        let titles: Vec<_> = store
            .load_pending(subscribe.id)
            .map_err(store_error)?
            .into_iter()
            .map(|(_, pending)| pending.torrent.title)
            .collect();
        Ok((media, files.len(), bytes, titles))
    })
    .await;
    let (media, file_count, bytes, mut titles) = match captured {
        Ok(Ok(captured)) => captured,
        Ok(Err(response)) => return response,
        Err(error) => return error.response(),
    };
    let inferred = titles.is_empty();
    if inferred {
        let downloader = state.downloader.clone();
        titles = match state
            .http_work
            .run(
                crate::http::library::BROWSE_DEADLINE,
                move || match downloader.task_snapshots() {
                    Ok(snapshots) => snapshots
                        .into_iter()
                        .filter(|snapshot| {
                            !snapshot.tag.is_empty()
                                && (downloader::names_match(&snapshot.name, &media.title)
                                    || media.original_title.as_deref().is_some_and(|original| {
                                        downloader::names_match(&snapshot.name, original)
                                    }))
                        })
                        .map(|snapshot| snapshot.name)
                        .collect(),
                    Err(error) => {
                        tracing::error!(%error, "Subscribe preview Downloader snapshot failed");
                        Vec::new()
                    }
                },
            )
            .await
        {
            Ok(titles) => titles,
            Err(error) => return crate::http::library::browse_work_error(error),
        };
    }
    ok(
        json!({"library_file_count":file_count, "library_bytes":bytes, "torrent_count":titles.len(),
        "torrent_count_inferred":inferred && !titles.is_empty(), "torrent_titles":titles}),
    )
    .into_response()
}

fn store_error(error: crate::store::StoreError) -> Response {
    tracing::error!(%error, "Subscribe removal preview capture failed");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}
