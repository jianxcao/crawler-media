//! Exact visible Library file and watch-unit identity.
use serde_json::Value;

use super::resolve_media_id;
use crate::store::UNIT_WHOLE;

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

pub(super) fn row_for(
    store: &crate::Store,
    body: &Value,
    user_id: Option<domain::UserId>,
) -> Option<(domain::LedgerRow, domain::Media, i32, i32)> {
    let selected = select_row(store, body, user_id);
    if selected.is_none() {
        tracing::error!(media_item_id = ?body.get("media_item_id"), file_id = ?body.get("file_id"),
            season = ?body.get("season_number").or_else(|| body.get("season")),
            episode = ?body.get("episode_number").or_else(|| body.get("episode")),
            "Playback file/unit selection rejected");
    }
    selected
}

fn select_row(
    store: &crate::Store,
    body: &Value,
    user_id: Option<domain::UserId>,
) -> Option<(domain::LedgerRow, domain::Media, i32, i32)> {
    let media_id = resolve_media_id(body["media_item_id"].as_str()?)?;
    let media = store.get_media(media_id).ok().flatten()?;
    let season = coordinate(body, "season_number", "season")?;
    let episode = coordinate(body, "episode_number", "episode")?;
    let whole_file = media.kind != domain::MediaKind::Tv;
    let matches = |row: &domain::LedgerRow| {
        let unit_matches = if whole_file {
            season.is_none_or(|s| s == 0) && episode.is_none_or(|e| e == 0)
        } else {
            season.is_none_or(|s| row.season == Some(s))
                && episode.is_none_or(|e| row.episode == Some(e))
        };
        row.media_id == media_id
            && unit_matches
            && crate::http::library::row_visible_to_user(store, row, &media, user_id)
    };
    let row = match body.get("file_id") {
        None | Some(Value::Null) => store.list_ledger().ok()?.into_iter().find(matches)?,
        Some(value) => {
            let row = store
                .get_ledger(&value.as_str()?.replace('-', ""))
                .ok()
                .flatten()?;
            if !matches(&row) {
                return None;
            }
            row
        }
    };
    // Whole-file media (movies, videos) use (UNIT_WHOLE, UNIT_WHOLE) for unit coordinates
    // in the ledger and unit_state tables. If caller requested (0, 0) or omitted coordinates,
    // they represent the entire movie/video unit.
    if whole_file {
        return Some((row, media, UNIT_WHOLE, UNIT_WHOLE));
    }
    selected_unit(row, media)
}

fn selected_unit(
    row: domain::LedgerRow,
    media: domain::Media,
) -> Option<(domain::LedgerRow, domain::Media, i32, i32)> {
    let season = row
        .season
        .map(i32::try_from)
        .transpose()
        .ok()?
        .unwrap_or(UNIT_WHOLE);
    let episode = row
        .episode
        .map(i32::try_from)
        .transpose()
        .ok()?
        .unwrap_or(UNIT_WHOLE);
    Some((row, media, season, episode))
}

/// Frontend movie units use (0, 0); persisted movie/whole-file units use (-1, -1).
/// TV specials keep (0, 0) as a real season/episode pair.
pub(super) fn watch_unit(
    store: &crate::Store,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> (i32, i32) {
    if (season, episode) != (0, 0) {
        return (season, episode);
    }
    match store.get_media(media_id).ok().flatten() {
        Some(media) if media.kind != domain::MediaKind::Tv => (UNIT_WHOLE, UNIT_WHOLE),
        _ => (season, episode),
    }
}

pub(super) fn start_position(body: &Value, watch: &Value) -> Result<i64, &'static str> {
    match body.get("start_ms") {
        Some(value) => value
            .as_i64()
            .filter(|n| *n >= 0)
            .ok_or("start_ms must be a nonnegative integer"),
        None if watch["played"].as_bool().unwrap_or(false) => Ok(0),
        None => Ok(watch["position_ms"].as_i64().unwrap_or(0).max(0)),
    }
}
