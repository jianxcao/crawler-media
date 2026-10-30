//! Person metadata is selected from published indexed facts, without Library enumeration.
use super::ApiServerProvider;
use domain::UserId;
use media_server::provider::MediaPerson;

pub(super) async fn deliver_person_image(
    provider: &ApiServerProvider,
    user: Option<UserId>,
    query: &str,
    headers: axum::http::HeaderMap,
) -> Result<Option<axum::response::Response>, String> {
    let Some(url) = resolve_person_image_url(provider, user, query).await? else {
        return Ok(None);
    };
    let response = provider
        .state
        .image_delivery
        .response_for_url(url.clone(), headers)
        .await;
    // The shared origin fill owns no Store lease. Permission and selected source
    // must still hold before publishing either bytes or a conditional response.
    if resolve_person_image_url(provider, user, query)
        .await?
        .as_ref()
        != Some(&url)
    {
        return Ok(None);
    }
    Ok(Some(response))
}

pub(super) async fn resolve_person_image_url(
    provider: &ApiServerProvider,
    user: Option<UserId>,
    query: &str,
) -> Result<Option<String>, String> {
    Ok(read_person(provider, user, query)
        .await?
        .and_then(|person| person.primary_image_url))
}
pub(super) async fn resolve_person(
    provider: &ApiServerProvider,
    user: UserId,
    query: &str,
) -> Result<Option<MediaPerson>, String> {
    read_person(provider, Some(user), query).await
}
async fn read_person(
    provider: &ApiServerProvider,
    user: Option<UserId>,
    query: &str,
) -> Result<Option<MediaPerson>, String> {
    let query = query.to_owned();
    provider
        .run_sync(move |provider| {
            provider.bounded_reader(|store| {
                Ok(store
                    .protocol_person(user, &query)
                    .map_err(|error| error.to_string())?
                    .map(|person| MediaPerson {
                        id: uuid::Uuid::parse_str(&person.wire_id)
                            .map(|id| id.hyphenated().to_string())
                            .unwrap_or(person.wire_id),
                        name: person.name,
                        person_type: person.person_type,
                        role: person.role,
                        tmdb_id: person.tmdb_id,
                        primary_image_url: person
                            .image_url
                            .as_deref()
                            .and_then(super::item_metadata::person_thumb_url),
                    }))
            })
        })
        .await?
}
