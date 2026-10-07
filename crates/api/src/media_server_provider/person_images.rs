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
    if let Some(user_id) = user_id {
        if let Some(person) = resolve_person(provider, user_id, query).await?
            && let Some(image_url) = person.primary_image_url
        {
            return Ok(Some(image_url));
        }
    }

    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    {
        let store = provider.state.store.lock();
        for row in store.list_ledger().map_err(|error| error.to_string())? {
            let Some(media) = store.get_media(row.media_id).ok().flatten() else {
                continue;
            };
            if !crate::http::library::row_visible_to_user(&store, &row, &media, user_id) {
                continue;
            }
            if let Some(image_url) = person_image_url(&row, &media, query) {
                return Ok(Some(image_url));
            }
            let Some(media_id) = media.tmdb_id.clone() else {
                continue;
            };
            let person_id = if uuid::Uuid::parse_str(query).is_ok() {
                Some(query.to_string())
            } else {
                person_tmdb_id(&row, &media, query).flatten()
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
    Ok(resolve_catalog_person_image(provider, query, candidates).await)
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
    candidates: Vec<(MediaKind, String, Option<String>)>,
) -> Option<String> {
    for (kind, media_id, person_id) in candidates {
        let state = provider.state.clone();
        let lookup_id = media_id.clone();
        let image_metadata = tokio::task::spawn_blocking(move || {
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
