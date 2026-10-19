use super::*;

pub(super) fn collect_server_read(
    store: &crate::Store,
    user_id: UserId,
    parent_id: Option<&str>,
    library_scope: Option<&str>,
) -> Result<ServerReadSnapshot, String> {
    let ownership = store.library_ownership().map_err(|error| error.to_string())?;
    let is_admin = store.user_role(user_id).map_err(|error| error.to_string())? == "admin";
    let rows = match library_scope {
        Some(library_id) => {
            let rows = store.ledger_candidates_for_library(&ownership, library_id).map_err(|error| error.to_string())?;
            match parent_id {
                Some(parent_id) if ownership.library(parent_id).is_none() => {
                    match parent_id.parse::<domain::MediaId>() {
                        Ok(media_id) => rows.into_iter().filter(|row| row.media_id == media_id).collect(),
                        Err(_) => Vec::new(),
                    }
                }
                _ => rows,
            }
        }
        None => store.jellyfin_ledger_candidates(&ownership, parent_id).map_err(|error| error.to_string())?,
    };
    let ids: Vec<_> = rows.iter().map(|row| row.media_id).collect();
    // Published, present file facts for the candidate rows: a pending or
    // missing file is not library content and never becomes an item.
    let file_ids: Vec<_> = rows.iter().map(|row| row.id).collect();
    let files = store
        .published_files(&file_ids)
        .map_err(|error| error.to_string())?;
    let media = store.media_batch(&ids).map_err(|error| error.to_string())?;
    let units = store
        .playback_units_for_media(user_id, &ids)
        .map_err(|error| error.to_string())?;
    Ok(ServerReadSnapshot { ownership, is_admin, media, rows, units, files })
}
