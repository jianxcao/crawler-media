//! Exact visible Library file and watch-unit identity.
use domain::{LedgerId, PhysicalSourceId, TypedItemId, UnitKey};
use serde_json::Value;

use super::request::{PlaybackSelection, SelectionError, parse_selection};
use crate::store::{ResolvedResume, UNIT_WHOLE};

/// The watch item a selection names. A movie/video owns only its whole work, so any
/// explicit coordinate is invalid (R13); a TV request without coordinates takes the
/// explicit file's recorded unit.
fn target_item(
    store: &crate::Store,
    media: &domain::Media,
    selection: &PlaybackSelection,
) -> Result<TypedItemId, SelectionError> {
    let media_id = media.id;
    let coordinates = (selection.season_number, selection.episode_number);
    if media.kind != domain::MediaKind::Tv {
        return match coordinates {
            (None, None) => Ok(TypedItemId::Work(media_id)),
            _ => Err(SelectionError::Invalid),
        };
    }
    Ok(match coordinates {
        (Some(s), Some(e)) => TypedItemId::Unit {
            media: media_id,
            key: UnitKey::new(s as i32, e as i32),
        },
        (None, None) => match inferred_unit(store, selection.file_id)? {
            Some((s, e)) => TypedItemId::Unit {
                media: media_id,
                key: UnitKey::new(s, e),
            },
            None => TypedItemId::Work(media_id),
        },
        _ => return Err(SelectionError::Invalid),
    })
}

fn selected_media(
    store: &crate::Store,
    selection: &PlaybackSelection,
) -> Result<domain::Media, SelectionError> {
    store
        .get_media(selection.media_item_id)
        .ok()
        .flatten()
        .ok_or(SelectionError::Unavailable)
}

/// Resolve a session request. An explicit source must be visible, present and cover
/// the target, or the request is refused; only an omitted source is chosen automatically.
pub(super) fn resolve_playback_for(
    store: &crate::Store,
    selection: &PlaybackSelection,
    user_id: domain::UserId,
) -> Result<(domain::LibraryFile, domain::Media, ResolvedResume), SelectionError> {
    let media = selected_media(store, selection)?;
    let item = target_item(store, &media, selection)?;
    let preferred_source = match selection.file_id {
        None => None,
        Some(file_id) => {
            let file = store
                .visible_file(user_id, file_id)
                .ok()
                .flatten()
                .ok_or(SelectionError::Unavailable)?;
            Some(PhysicalSourceId {
                file_id,
                revision: file.version.revision,
            })
        }
    };
    let resolved = store
        .resolve_playback(
            user_id,
            &item,
            preferred_source.as_ref(),
            selection.start_ms,
        )
        .map_err(|_| SelectionError::Unavailable)?;
    if preferred_source.is_some_and(|pref| resolved.target.file.file_id != pref.file_id) {
        return Err(SelectionError::Unavailable);
    }
    Ok((resolved.target.file.clone(), media, resolved))
}

/// Unit recorded for a selected file, used when a TV request omits coordinates.
fn inferred_unit(
    store: &crate::Store,
    file_id: Option<LedgerId>,
) -> Result<Option<(i32, i32)>, SelectionError> {
    let Some(file_id) = file_id else {
        return Ok(None);
    };
    let coverage = store
        .file_coverage_by_id(file_id)
        .ok()
        .flatten()
        .ok_or(SelectionError::Unavailable)?;
    let [unit] = coverage.units.as_slice() else {
        return Err(SelectionError::Invalid);
    };
    Ok((!unit.is_whole()).then_some((unit.season, unit.episode)))
}

/// Resolve the source a heartbeat/decide request names, with the same contract.
pub(super) fn row_for(
    store: &crate::Store,
    body: &Value,
    user_id: Option<domain::UserId>,
) -> Result<(domain::LibraryFile, domain::Media, i32, i32), SelectionError> {
    let selection = parse_selection(body)?;
    let media = selected_media(store, &selection)?;
    let item = target_item(store, &media, &selection)?;
    let entries = match user_id {
        Some(user_id) => store.playable_entries(user_id, &item),
        None => store.published_entries_for_item(&item),
    }
    .map_err(|_| SelectionError::Unavailable)?;
    let source = match selection.file_id {
        Some(fid) => entries.into_iter().find(|s| s.file.file_id == fid),
        None => entries.into_iter().next(),
    }
    .ok_or(SelectionError::Unavailable)?;
    let unit = (source.unit.season, source.unit.episode);
    Ok((source.file, media, unit.0, unit.1))
}

/// The stored unit a public watch-state request names. A movie owns only the
/// whole-media unit: any other coordinate (including the historical 0/0) names
/// nothing and is refused instead of aliased (R13).
pub(super) fn watch_unit(
    store: &crate::Store,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> Option<(i32, i32)> {
    if (season, episode) == (UNIT_WHOLE, UNIT_WHOLE) {
        return Some((UNIT_WHOLE, UNIT_WHOLE));
    }
    let is_movie = store
        .get_media(media_id)
        .ok()
        .flatten()
        .is_some_and(|media| media.kind != domain::MediaKind::Tv);
    (!is_movie).then_some((season, episode))
}
