use super::*;

pub(super) fn collect_server_read(
    store: &crate::Store,
    user_id: UserId,
    parent_id: Option<&str>,
    library_scope: Option<&str>,
) -> Result<ServerReadSnapshot, String> {
    let ownership = store
        .library_ownership()
        .map_err(|error| error.to_string())?;
    let is_admin = store
        .user_role(user_id)
        .map_err(|error| error.to_string())?
        == "admin";
    let coverages = match library_scope {
        Some(library_id) => {
            let coverages = store
                .ledger_candidates_for_library(&ownership, library_id)
                .map_err(|error| error.to_string())?;
            match parent_id {
                Some(parent_id) if ownership.library(parent_id).is_none() => {
                    match domain::TypedItemId::protocol_media_id(parent_id) {
                        Some(media_id) => coverages
                            .into_iter()
                            .filter(|coverage| coverage.file.media_id == media_id)
                            .collect(),
                        None => Vec::new(),
                    }
                }
                _ => coverages,
            }
        }
        None => store
            .jellyfin_ledger_candidates(&ownership, parent_id)
            .map_err(|error| error.to_string())?,
    };
    let ids: Vec<_> = coverages.iter().map(|c| c.file.media_id).collect();
    // Published, present file facts for the candidate coverages: a pending or
    // missing file is not library content and never becomes an item.
    let file_ids: Vec<_> = coverages.iter().map(|c| c.file.file_id).collect();
    let files = store
        .published_files(&file_ids)
        .map_err(|error| error.to_string())?;
    let media = store.media_batch(&ids).map_err(|error| error.to_string())?;
    let units = store
        .playback_units_for_media(user_id, &ids)
        .map_err(|error| error.to_string())?;
    Ok(ServerReadSnapshot {
        ownership,
        is_admin,
        media,
        coverages,
        units,
        files,
    })
}
