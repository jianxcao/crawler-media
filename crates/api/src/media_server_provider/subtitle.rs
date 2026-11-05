use super::*;

pub(super) async fn resolve_snapshot(
    provider: &ApiServerProvider,
    user_id: UserId,
    request: &media_server::provider::SubtitleRequest<'_>,
) -> Result<Option<MediaItemSnapshot>, SubtitleDeliveryError> {
    let Some(source) = request
        .media_source_id
        .and_then(domain::PhysicalSourceId::from_wire_string)
    else {
        return Ok(None);
    };
    let item_id = request.item_id.to_owned();
    let session = request.play_session_id.map(str::to_owned);
    let device_id = request.device_id.to_owned();
    provider
        .run_sync(move |provider| {
            let entry =
                provider.bounded_reader(&[store::Db::Library, store::Db::App], |store| {
                    items::resolve_entry_with_source(store, Some(user_id), &item_id, Some(&source))
                })?;
            let Some(entry) = entry else { return Ok(None) };
            if let Some(session) = session.as_deref()
                && !provider.stream_matches_binding(
                    user_id,
                    Some(&device_id),
                    session,
                    &entry,
                    &entry.file,
                )?
            {
                return Ok(None);
            }
            items::snapshot_from_entry(
                &provider.state,
                user_id,
                entry.file,
                entry.unit,
                entry.media,
                entry.is_series,
            )
            .map(Some)
        })
        .await
        .map_err(SubtitleDeliveryError::Failed)?
        .map_err(SubtitleDeliveryError::Failed)
}
