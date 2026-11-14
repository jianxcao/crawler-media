use super::*;

pub(super) fn mark_error(
    error: crate::store::StoreError,
    user_id: UserId,
    media_id: domain::MediaId,
) -> UserMarksError {
    match error {
        crate::store::StoreError::WriteBusy
        | crate::store::StoreError::ReadBusy
        | crate::store::StoreError::ReadClosed
        | crate::store::StoreError::ReadDeadline
        | crate::store::StoreError::ReadCancelled => UserMarksError::Unavailable,
        crate::store::StoreError::Protected(message) => {
            tracing::warn!(%message, %user_id, %media_id, "Jellyfin user mark rejected by write barrier");
            UserMarksError::Conflict(message)
        }
        other => {
            tracing::error!(error = %other, %user_id, %media_id, "failed to update Jellyfin user marks");
            UserMarksError::Internal(other.to_string())
        }
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
) -> Result<Option<(domain::MediaId, i32, i32)>, String> {
    let Some(item) = domain::TypedItemId::from_wire_string(id) else {
        return Ok(None);
    };
    // Marks are writes: the named item must still be visible content for this
    // user, so a removed library, pending file or unknown id resolves to None.
    let visible = store
        .playable_sources(user_id, &item)
        .map_err(|error| error.to_string())?;
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
