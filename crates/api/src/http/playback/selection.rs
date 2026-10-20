//! Exact visible Library file and watch-unit identity.
use domain::{LedgerId, PhysicalSourceId, TypedItemId, UnitKey};
use serde_json::Value;

use super::resolve_media_id;
use crate::store::{ResolvedResume, UNIT_WHOLE};

fn coordinate(body: &Value, key: &str, alias: &str) -> Option<Option<u32>> {
    match body.get(key).or_else(|| body.get(alias)) {
        None | Some(Value::Null) => Some(None),
        Some(value) => value
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| i32::try_from(*n).is_ok())
            .map(Some),
    }
}

pub(super) fn resolve_playback_for(
    store: &crate::Store,
    body: &Value,
    user_id: domain::UserId,
) -> Option<(domain::LedgerRow, domain::Media, ResolvedResume)> {
    let media_id = resolve_media_id(body["media_item_id"].as_str()?)?;
    let media = store.get_media(media_id).ok().flatten()?;
    let season = coordinate(body, "season_number", "season")?;
    let episode = coordinate(body, "episode_number", "episode")?;
    let whole_file = media.kind != domain::MediaKind::Tv;
    // Movie requests use the canonical whole-work identity regardless of legacy
    // zero coordinates; nonzero coordinates cannot name a movie unit.
    if whole_file && (season.is_some_and(|value| value != 0)
        || episode.is_some_and(|value| value != 0)) {
        return None;
    }

    let preferred_file_id = body
        .get("file_id")
        .and_then(|v| v.as_str())
        .and_then(|raw| raw.replace('-', "").parse::<LedgerId>().ok());

    let item = if whole_file {
        TypedItemId::Work(media_id)
    } else {
        match (season, episode) {
            (Some(s), Some(e)) => TypedItemId::Unit {
                media: media_id,
                key: UnitKey::new(s as i32, e as i32),
            },
            // Omitted coordinates: infer the selected file's unit instead of
            // asking for a Work the TV media does not have.
            _ => match inferred_unit(store, preferred_file_id) {
                Some((s, e)) => TypedItemId::Unit {
                    media: media_id,
                    key: UnitKey::new(s, e),
                },
                None => TypedItemId::Work(media_id),
            },
        }
    };

    let preferred_source = preferred_file_id
        .and_then(|file_id| {
            let file = store.visible_file(user_id, file_id).ok().flatten()?;
            Some(PhysicalSourceId {
                file_id,
                revision: file.version.revision,
            })
        });

    let explicit_start_ms = body
        .get("start_ms")
        .and_then(|v| v.as_i64())
        .filter(|n| *n >= 0);

    let resolved = store
        .resolve_playback(user_id, &item, preferred_source.as_ref(), explicit_start_ms)
        .ok()?;

    let unit = (resolved.target.unit.season, resolved.target.unit.episode);
    let row = crate::media_server_provider::items::row_view(&resolved.target.file, unit);

    // If caller specified file_id, verify that the resolved file matches it
    if let Some(pref) = preferred_source {
        if resolved.target.file.file_id != pref.file_id {
            return None;
        }
    }

    Some((row, media, resolved))
}

/// Unit recorded for a selected file, used when a TV request omits coordinates.
fn inferred_unit(store: &crate::Store, file_id: Option<LedgerId>) -> Option<(i32, i32)> {
    let row = store.get_ledger(&file_id?.to_string()).ok().flatten()?;
    Some((row.season? as i32, row.episode? as i32))
}

pub(super) fn row_for(
    store: &crate::Store,
    body: &Value,
    user_id: Option<domain::UserId>,
) -> Option<(domain::LedgerRow, domain::Media, i32, i32)> {
    let media_id = resolve_media_id(body["media_item_id"].as_str()?)?;
    let media = store.get_media(media_id).ok().flatten()?;
    let season = coordinate(body, "season_number", "season")?;
    let episode = coordinate(body, "episode_number", "episode")?;
    let whole_file = media.kind != domain::MediaKind::Tv;
    // A movie owns exactly one canonical whole-media unit. Explicit nonzero
    // coordinates name a unit that cannot exist; resolve nothing instead of
    // silently aliasing them onto the whole movie.
    if whole_file && (season.is_some_and(|value| value != 0)
        || episode.is_some_and(|value| value != 0)) {
        return None;
    }

    let pref_file_id = body
        .get("file_id")
        .and_then(|v| v.as_str())
        .and_then(|raw| raw.replace('-', "").parse::<LedgerId>().ok());

    let item = if whole_file {
        TypedItemId::Work(media_id)
    } else {
        match (season, episode) {
            (Some(s), Some(e)) => TypedItemId::Unit {
                media: media_id,
                key: UnitKey::new(s as i32, e as i32),
            },
            _ => match inferred_unit(store, pref_file_id) {
                Some((s, e)) => TypedItemId::Unit {
                    media: media_id,
                    key: UnitKey::new(s, e),
                },
                None => TypedItemId::Work(media_id),
            },
        }
    };

    let entries = match user_id {
        Some(user_id) => store.playable_entries(user_id, &item).ok()?,
        None => store.published_entries_for_item(&item).ok()?,
    };

    let source = if let Some(fid) = pref_file_id {
        entries.into_iter().find(|s| s.file.file_id == fid)?
    } else {
        entries.into_iter().next()?
    };

    let unit = (source.unit.season, source.unit.episode);
    let row = crate::media_server_provider::items::row_view(&source.file, unit);
    Some((row, media, unit.0, unit.1))
}

pub(super) fn watch_unit(
    store: &crate::Store,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> (i32, i32) {
    if (season, episode) == (UNIT_WHOLE, UNIT_WHOLE) {
        return (UNIT_WHOLE, UNIT_WHOLE);
    }
    // A movie owns exactly one canonical whole-media unit. Clients that still send the
    // historical zero coordinates name that same unit; they never name a stored (0, 0) row.
    if (season, episode) == (0, 0)
        && store
            .get_media(media_id)
            .ok()
            .flatten()
            .is_some_and(|media| media.kind != domain::MediaKind::Tv)
    {
        return (UNIT_WHOLE, UNIT_WHOLE);
    }
    (season, episode)
}
