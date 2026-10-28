use std::collections::HashSet;

use domain::{MediaKind, UserId};
use media_server::provider::{MediaPerson, MediaServerProvider};

use super::ApiServerProvider;
use super::item_metadata::{catalog_person_image_url, person_image_url, person_tmdb_id};

pub(super) async fn resolve_person_image_url(
    provider: &ApiServerProvider,
    user_id: Option<UserId>,
    query: &str,
) -> Result<Option<String>, String> {
    if let Some(user_id) = user_id
        && let Some(person) = resolve_person(provider, user_id, query).await?
        && let Some(image_url) = person.primary_image_url
    {
        return Ok(Some(image_url));
    }

    let captured = crate::http::store_wait::store_read(&provider.state, move |store| {
        capture_visible_files(store, user_id)
    })
    .await
    .map_err(|error| error.to_string())??;
    let query_owned = query.to_string();
    let local = provider
        .state
        .fs_work
        .run(crate::http::library::BROWSE_DEADLINE, move || {
            observe_people(captured, &query_owned)
        })
        .await
        .map_err(|error| error.to_string())?;
    if local.image.is_some() {
        return Ok(local.image);
    }
    Ok(resolve_catalog_person_image(provider, query, local.candidates).await)
}

type PersonSource = (domain::FileCoverage, domain::Media);
type CatalogCandidate = (MediaKind, String, Option<String>);

struct LocalPeople {
    image: Option<String>,
    candidates: Vec<CatalogCandidate>,
}

fn capture_visible_files(
    store: &crate::Store,
    user: Option<UserId>,
) -> Result<Vec<PersonSource>, String> {
    let ownership = store
        .library_ownership()
        .map_err(|error| error.to_string())?;
    let is_admin = user
        .map(|user| store.user_role(user))
        .transpose()
        .map_err(|error| error.to_string())?
        .is_some_and(|role| role == "admin");
    let visible: HashSet<_> = ownership
        .libraries()
        .iter()
        .filter(|library| {
            ownership.accepts_work(&library.id)
                && match user {
                    None => true,
                    Some(_) if is_admin => library.admin_visible,
                    Some(user) => {
                        library.access_mode == "everyone" || library.member_ids.contains(&user)
                    }
                }
        })
        .map(|library| library.id.clone())
        .collect();
    let files: Vec<_> = store
        .list_file_coverage()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|row| visible.contains(&row.file.library_id.to_string()))
        .collect();
    let ids: HashSet<_> = files.iter().map(|row| row.file.media_id).collect();
    let media = store
        .media_batch(&ids.into_iter().collect::<Vec<_>>())
        .map_err(|error| error.to_string())?;
    Ok(files
        .into_iter()
        .filter_map(|row| {
            media
                .get(&row.file.media_id)
                .cloned()
                .map(|media| (row, media))
        })
        .collect())
}

fn observe_people(files: Vec<PersonSource>, query: &str) -> LocalPeople {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    let query_is_id = uuid::Uuid::parse_str(query).is_ok();
    for (coverage, media) in files {
        let file = &coverage.file;
        // Native NFO reads run on the filesystem worker after the database snapshot is released.
        for unit in super::items::coverage_units(&coverage) {
            if let Some(image) = person_image_url(file, unit, &media, query) {
                return LocalPeople {
                    image: Some(image),
                    candidates,
                };
            }
            let Some(media_id) = media.tmdb_id.clone() else {
                continue;
            };
            let person_id = if query_is_id {
                Some(query.to_string())
            } else {
                person_tmdb_id(file, unit, &media, query).flatten()
            };
            let key = (
                media.kind.as_str().to_string(),
                media_id.clone(),
                person_id.clone(),
            );
            if seen.insert(key) {
                candidates.push((media.kind, media_id, person_id));
            }
        }
    }
    LocalPeople {
        image: None,
        candidates,
    }
}

pub(super) async fn resolve_person(
    provider: &ApiServerProvider,
    user_id: UserId,
    query: &str,
) -> Result<Option<MediaPerson>, String> {
    let items = provider.list_visible_items(user_id, None).await?;
    let mut seen_media = HashSet::new();
    let mut items_to_enrich = Vec::new();
    for item in items {
        if !seen_media.insert(item.media.id) {
            continue;
        }
        if let Some(person) = item
            .metadata
            .people
            .iter()
            .find(|person| person_matches_query(person, query))
        {
            return Ok(Some(person.clone()));
        }
        items_to_enrich.push(item);
    }

    for item in items_to_enrich {
        let item = provider.enrich_single_item(item).await;
        if let Some(person) = item
            .metadata
            .people
            .into_iter()
            .find(|person| person_matches_query(person, query))
        {
            return Ok(Some(person));
        }
    }
    Ok(None)
}

async fn resolve_catalog_person_image(
    provider: &ApiServerProvider,
    query: &str,
    candidates: Vec<CatalogCandidate>,
) -> Option<String> {
    for (kind, media_id, person_id) in candidates {
        let state = provider.state.clone();
        let lookup_id = media_id.clone();
        let image_metadata = provider
            .state
            .http_work
            .run(crate::http::library::BROWSE_DEADLINE, move || {
                crate::scrape_metadata::fetch_tmdb_metadata(&state, kind, &lookup_id)
            })
            .await;
        let metadata = match image_metadata {
            Ok(Ok(Some(metadata))) => metadata,
            Ok(Ok(None)) => continue,
            Ok(Err(error)) => {
                tracing::warn!(%error, %media_id, person = %query, "failed to look up Jellyfin person image metadata");
                continue;
            }
            Err(error) => {
                tracing::warn!(%error, %media_id, person = %query, "Jellyfin person image lookup task failed");
                continue;
            }
        };
        if let Some(url) = catalog_person_image_url(&metadata, query, person_id.as_deref()) {
            return Some(url);
        }
    }
    None
}

fn person_matches_query(person: &MediaPerson, query: &str) -> bool {
    person.name.eq_ignore_ascii_case(query)
        || person
            .id
            .replace('-', "")
            .eq_ignore_ascii_case(&query.replace('-', ""))
        || person
            .tmdb_id
            .as_deref()
            .is_some_and(|id| id.eq_ignore_ascii_case(query))
}
