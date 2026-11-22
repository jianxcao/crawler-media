use super::*;

pub(super) fn mark_error(
    error: crate::store::StoreError,
    user_id: UserId,
    media_id: domain::MediaId,
) -> Result<bool, UserMarksError> {
    match error {
        crate::store::StoreError::Missing(_) => Ok(false),
        crate::store::StoreError::WriteBusy
        | crate::store::StoreError::ReadBusy
        | crate::store::StoreError::ReadClosed
        | crate::store::StoreError::ReadDeadline
        | crate::store::StoreError::ReadCancelled => Err(UserMarksError::Unavailable),
        crate::store::StoreError::Protected(message) => {
            tracing::warn!(%message, %user_id, %media_id, "Jellyfin user mark rejected by write barrier");
            Err(UserMarksError::Conflict(message))
        }
        other => {
            tracing::error!(error = %other, %user_id, %media_id, "failed to update Jellyfin user marks");
            Err(UserMarksError::Internal(other.to_string()))
        }
    }
}

pub(super) fn snapshot_error(error: crate::store::StoreError) -> UserMarksError {
    match error {
        crate::store::StoreError::WriteBusy
        | crate::store::StoreError::ReadBusy
        | crate::store::StoreError::ReadClosed
        | crate::store::StoreError::ReadDeadline
        | crate::store::StoreError::ReadCancelled => UserMarksError::Unavailable,
        other => UserMarksError::Internal(other.to_string()),
    }
}

pub(super) fn unit_played(
    store: &crate::Store,
    user_id: UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> bool {
    match store.effective_unit_state(user_id, media_id, domain::UnitKey::new(season, episode)) {
        Ok(state) => state.is_some_and(|unit| unit.played),
        Err(error) => {
            tracing::warn!(%error, %user_id, %media_id, season, episode, "failed to read Jellyfin played mark");
            false
        }
    }
}

/// C05: the newest explicit assertion among episode/season/whole decides
/// (`played_changed_at_ms`; equal times go to the more specific scope). The
/// activity clock never ranks marks. Without any assertion the unit's own row
/// (automatic completion) decides.
pub(super) fn resolve_effective_played(
    episode_unit: Option<(&crate::store::UnitState, i64)>,
    season_unit: Option<(&crate::store::UnitState, i64)>,
    show_unit: Option<(&crate::store::UnitState, i64)>,
) -> bool {
    let asserted = [episode_unit, season_unit, show_unit]
        .into_iter()
        .enumerate()
        .filter_map(|(specificity, unit)| {
            let (state, _) = unit?;
            state
                .played_assertion_at_ms
                .map(|at| (at, 2 - specificity, state.played))
        })
        .max_by_key(|(at, specificity, _)| (*at, *specificity));
    match asserted {
        Some((_, _, played)) => played,
        None => episode_unit.is_some_and(|(state, _)| state.played),
    }
}
pub(super) fn item_is_played(
    store: &crate::Store,
    user_id: UserId,
    file: &domain::LibraryFile,
    unit: (i32, i32),
) -> bool {
    // 旧行模型的 `season/episode` 为 `None` 即整片；`UnitKey` 用 `-1` 表达同一件事。
    unit_played(store, user_id, file.media_id, unit.0, unit.1)
}

pub(super) fn unit_play_count(
    store: &crate::Store,
    user_id: UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> i64 {
    match store.unit_state(user_id, media_id, season, episode) {
        Ok(Some(state)) => state.play_count,
        Ok(None) => 0,
        Err(error) => {
            tracing::warn!(%error, %user_id, %media_id, season, episode, "failed to read Jellyfin play count");
            0
        }
    }
}
pub(super) fn resolve_mark_unit(
    store: &crate::Store,
    user_id: UserId,
    id: &str,
) -> Result<Option<(domain::MediaId, i32, i32)>, crate::store::StoreError> {
    let Some(item) = domain::TypedItemId::from_wire_string(id) else {
        return Ok(None);
    };
    // Marks are writes: the named item must still be visible content for this
    // user, so a removed library, pending file or unknown id resolves to None.
    let visible = store.playable_sources(user_id, &item)?;
    if visible.is_empty() {
        return Ok(None);
    }
    Ok(Some(match item {
        domain::TypedItemId::Work(media) => {
            (media, crate::store::UNIT_WHOLE, crate::store::UNIT_WHOLE)
        }
        domain::TypedItemId::Season { media, season } => {
            (media, season as i32, crate::store::UNIT_WHOLE)
        }
        domain::TypedItemId::Unit { media, key } => (media, key.season, key.episode),
    }))
}

/// Season-level marks rolled up over the season's visible episodes.
pub(super) async fn season_user_item_data(
    provider: &ApiServerProvider,
    user_id: UserId,
    media: domain::MediaId,
    season: u32,
) -> Result<media_server::provider::UserItemData, String> {
    let summary = super::entities::seasons(provider, user_id, media)
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|row| row.season == season);
    let (total, played) = summary
        .map(|row| (row.total as usize, row.played as usize))
        .unwrap_or_default();
    let favorite = provider
        .run_sync(move |provider| {
            provider.bounded_reader(&[store::Db::Library], |store| {
                store
                    .unit_state(user_id, media, season as i32, crate::store::UNIT_WHOLE)
                    .map(|state| state.is_some_and(|state| state.favorite))
                    .map_err(|error| error.to_string())
            })
        })
        .await??;
    let unplayed_item_count = total.saturating_sub(played);
    let item_id = domain::TypedItemId::Season { media, season }.to_wire_string();
    Ok(media_server::provider::UserItemData {
        key: item_id.clone(),
        item_id,
        playback_position_ticks: 0,
        play_count: 0,
        played: total == 0 || unplayed_item_count == 0,
        is_favorite: favorite,
        unplayed_item_count: Some(unplayed_item_count),
        played_percentage: (total > 0).then(|| played as f64 / total as f64 * 100.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_target_maps_to_not_found_outcome() {
        let result = mark_error(
            crate::store::StoreError::Missing("gone".into()),
            UserId::new(),
            domain::MediaId::new(),
        );
        assert!(!result.unwrap());
    }
}
