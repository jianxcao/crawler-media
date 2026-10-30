//! Protocol entities use published projections; only search result pages hydrate files.
use super::{ApiServerProvider, UserId};
use media_server::provider::{Entity, EntityKind, EntityPage, ItemCounts, Season, ServerItemPage};

fn kind(value: EntityKind) -> store::ProtocolEntityKind {
    match value {
        EntityKind::Genre => store::ProtocolEntityKind::Genre,
        EntityKind::Studio => store::ProtocolEntityKind::Studio,
    }
}

pub(super) async fn entities(
    provider: &ApiServerProvider,
    user: UserId,
    entity_kind: EntityKind,
    parent: Option<&str>,
    search: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<EntityPage, String> {
    let parent = parent.map(str::to_owned);
    let search = search.map(str::to_owned);
    provider
        .run_sync(move |provider| {
            provider.bounded_reader(|store| {
                let library_id = parent.and_then(|id| id.parse::<domain::LibraryId>().ok());
                let page = store
                    .protocol_entities(&store::ProtocolEntityQuery {
                        user_id: user,
                        library_id,
                        kind: kind(entity_kind),
                        search,
                        offset,
                        limit,
                    })
                    .map_err(|error| error.to_string())?;
                Ok(EntityPage {
                    total: page.total,
                    items: page
                        .items
                        .into_iter()
                        .map(|item| Entity {
                            name: item.name,
                            wire_id: hyphenated_id(item.wire_id),
                            child_count: item.child_count,
                        })
                        .collect(),
                })
            })
        })
        .await?
}
pub(super) async fn counts(
    provider: &ApiServerProvider,
    user: UserId,
) -> Result<ItemCounts, String> {
    provider
        .run_sync(move |provider| {
            provider.bounded_reader(|store| {
                let value = store
                    .protocol_counts(user)
                    .map_err(|error| error.to_string())?;
                Ok(ItemCounts {
                    movies: value.movies,
                    series: value.series,
                    episodes: value.episodes,
                    items: value.items,
                })
            })
        })
        .await?
}
pub(super) async fn seasons(
    provider: &ApiServerProvider,
    user: UserId,
    media: domain::MediaId,
) -> Result<Vec<Season>, String> {
    provider
        .run_sync(move |provider| {
            provider.bounded_reader(|store| {
                Ok(store
                    .protocol_seasons(user, media)
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .map(|row| Season {
                        season: row.season,
                        total: row.total,
                        played: row.played,
                    })
                    .collect())
            })
        })
        .await?
}
pub(super) async fn image(
    provider: &ApiServerProvider,
    user: Option<UserId>,
    entity_kind: EntityKind,
    query: &str,
) -> Result<Option<Vec<u8>>, String> {
    let query = query.to_owned();
    let lookup = query.clone();
    let captured = provider
        .run_sync(move |provider| {
            provider.bounded_reader(|store| {
                let Some(reference) = store
                    .protocol_entity_representative(user, kind(entity_kind), &lookup)
                    .map_err(|error| error.to_string())?
                else {
                    return Ok(None);
                };
                let Some(source) = reference.preferred_source.as_ref() else {
                    return Ok(None);
                };
                let Some(file) = store
                    .published_files(&[source.file_id])
                    .map_err(|error| error.to_string())?
                    .remove(&source.file_id)
                else {
                    return Ok(None);
                };
                if file.library_id != reference.library_id
                    || file.version.revision != source.revision
                {
                    return Ok(None);
                }
                Ok(Some((reference, file)))
            })
        })
        .await??;
    let Some((reference, file)) = captured else {
        return Ok(None);
    };
    let bytes = provider
        .state
        .fs_work
        .run(
            crate::http::library_artwork::image_response::IMAGE_READ,
            move || {
                use std::io::Read;
                let Some(path) = crate::http::library::poster_path(&file) else {
                    return Ok(None);
                };
                let handle = std::fs::File::open(path).map_err(|error| error.to_string())?;
                let mut bytes = Vec::new();
                handle
                    .take(8 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| error.to_string())?;
                if bytes.len() > 8 * 1024 * 1024 {
                    return Err("entity poster exceeds image response limit".to_owned());
                }
                Ok(Some(bytes))
            },
        )
        .await
        .map_err(|error| error.to_string())??;
    let unchanged = provider
        .run_sync(move |provider| {
            provider.bounded_reader(|store| {
                let current = store
                    .protocol_entity_representative(user, kind(entity_kind), &query)
                    .map_err(|error| error.to_string())?;
                Ok(current.as_ref() == Some(&reference))
            })
        })
        .await??;
    Ok(unchanged.then_some(bytes).flatten())
}
pub(super) async fn hints(
    provider: &ApiServerProvider,
    user: UserId,
    term: &str,
    offset: usize,
    limit: usize,
) -> Result<ServerItemPage, String> {
    super::selection::hints(provider, user, term, offset, limit).await
}

fn hyphenated_id(raw: String) -> String {
    uuid::Uuid::parse_str(&raw)
        .map(|id| id.hyphenated().to_string())
        .unwrap_or(raw)
}
