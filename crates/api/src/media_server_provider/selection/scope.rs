//! Lightweight request classification; continuation selection never expands coverage.
use domain::{Media, MediaId, MediaKind, UserId};
use media_server::provider::ServerItemQuery;
use media_server::provider::query::{
    folder_id_for_root, series_media_id as protocol_series_media_id,
};
use std::{collections::HashMap, path::PathBuf};

pub(super) struct ScopedRead {
    pub(super) user_id: UserId,
    pub(super) media: HashMap<MediaId, Media>,
    pub(super) requested_library: Option<String>,
    pub(super) folder_root: Option<PathBuf>,
    pub(super) media_scoped: bool,
    pub(super) unresolved: bool,
    pub(super) unknown_parent: bool,
}

pub(super) fn request_scope(
    store: &crate::Store,
    user_id: UserId,
    query: &ServerItemQuery,
) -> Result<ScopedRead, store::StoreError> {
    let is_admin = store.user_role(user_id)? == "admin";
    let libraries: Vec<_> = store
        .list_libraries()?
        .into_iter()
        .filter(|library| {
            if is_admin {
                library.admin_visible
            } else {
                library.access_mode == "everyone" || library.member_ids.contains(&user_id)
            }
        })
        .collect();
    let parent = query.parent_id.as_deref();
    let is_folder = parent.is_some_and(|value| value.starts_with("folder_"));
    let library = parent.filter(|id| libraries.iter().any(|library| library.id == *id));
    let folder = if is_folder {
        parent.and_then(|id| folder_root_in(id, &libraries))
    } else {
        None
    };
    let series_parent = if library.is_none() && !is_folder {
        parent
            .and_then(protocol_series_media_id)
            .map(|id| store.get_media(id))
            .transpose()?
            .flatten()
            .filter(|media| media.kind == MediaKind::Tv)
    } else {
        None
    };
    let target = if library.is_some() || is_folder {
        query.series_id.as_deref()
    } else if query.series_id.is_some() {
        parent.or(query.series_id.as_deref())
    } else if series_parent.is_some() {
        parent
    } else {
        None
    };
    let media_scope = target.and_then(protocol_series_media_id);
    let mut scope = ScopedRead {
        user_id,
        media: HashMap::new(),
        requested_library: None,
        folder_root: None,
        media_scoped: false,
        unresolved: false,
        unknown_parent: false,
    };
    scope.unresolved =
        (is_folder && folder.is_none()) || (target.is_some() && media_scope.is_none());
    scope.requested_library = library.map(str::to_owned);
    scope.folder_root = folder;
    scope.media_scoped = media_scope.is_some();
    scope.unknown_parent = parent.is_some() && library.is_none() && !is_folder && target.is_none();
    if let Some(media) = series_parent {
        scope.media.insert(media.id, media);
    }
    if let Some(media_id) = media_scope
        && !scope.media.contains_key(&media_id)
        && let Some(media) = store.get_media(media_id)?
    {
        scope.media.insert(media.id, media);
    }
    Ok(scope)
}

fn folder_root_in(parent: &str, libraries: &[crate::store::Library]) -> Option<PathBuf> {
    libraries.iter().find_map(|library| {
        library
            .root_paths
            .iter()
            .find_map(|root| (folder_id_for_root(root) == parent).then(|| root.clone()))
    })
}
