use domain::{LedgerRow, UserId};
use std::str::FromStr;

use crate::http::library::row_visible_to_user;

pub(crate) fn resolve_visible_row(
    store: &crate::Store,
    id: &str,
    user_id: Option<UserId>,
) -> Option<LedgerRow> {
    if let Ok(Some(row)) = store.get_ledger(id) {
        let media = store.get_media(row.media_id).ok().flatten()?;
        return row_visible_to_user(store, &row, &media, user_id).then_some(row);
    }

    let media_id = domain::MediaId::from_str(id).ok()?;
    store.list_ledger().ok()?.into_iter().find(|row| {
        if row.media_id != media_id {
            return false;
        }
        let Some(media) = store.get_media(row.media_id).ok().flatten() else {
            return false;
        };
        row_visible_to_user(store, row, &media, user_id)
    })
}

pub(crate) fn resolve_visible_season(
    store: &crate::Store,
    id: &str,
    user_id: Option<UserId>,
) -> Option<(LedgerRow, u32)> {
    let compact_id = id.replace('-', "");
    let media_id = domain::MediaId::from_str(compact_id.get(..32)?).ok()?;
    let season = compact_id.get(32..)?.parse::<u32>().ok()?;
    let rows = store.list_ledger().ok()?;
    rows.into_iter().find_map(|row| {
        if row.media_id != media_id || row.season != Some(season) {
            return None;
        }
        let media = store.get_media(row.media_id).ok().flatten()?;
        (media.kind == domain::MediaKind::Tv && row_visible_to_user(store, &row, &media, user_id))
            .then_some((row, season))
    })
}
