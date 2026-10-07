use domain::Media;
use media_server::provider::MediaPerson;

use super::nfo::row_nfo_metadata;

pub(super) fn merge_people(people: &mut Vec<MediaPerson>, catalog_people: Vec<MediaPerson>) {
    for catalog_person in catalog_people {
        if let Some(person) = people
            .iter_mut()
            .find(|person| same_person(person, &catalog_person))
        {
            if person.role.as_deref().is_none_or(str::is_empty) {
                person.role = catalog_person.role;
            }
            if person.tmdb_id.is_none() {
                person.tmdb_id = catalog_person.tmdb_id;
            }
            if person.primary_image_url.is_none() {
                person.primary_image_url = catalog_person.primary_image_url;
            }
        } else {
            people.push(catalog_person);
        }
    }
}

fn same_person(left: &MediaPerson, right: &MediaPerson) -> bool {
    if left.person_type != right.person_type {
        return false;
    }
    match (left.tmdb_id.as_deref(), right.tmdb_id.as_deref()) {
        (Some(left_id), Some(right_id)) => left_id == right_id,
        _ => left.name.trim().to_lowercase() == right.name.trim().to_lowercase(),
    }
}

pub(super) fn people_from_nfo(metadata: Option<&library::NfoMeta>) -> Vec<MediaPerson> {
    let mut seen = std::collections::HashSet::new();
    let Some(metadata) = metadata else {
        return Vec::new();
    };
    let mut people = metadata
        .cast
        .iter()
        .map(|member| {
            (
                member.name.as_str(),
                member.role.as_deref(),
                "Actor",
                member.tmdb_id.as_deref(),
                member.thumb.as_deref(),
            )
        })
        .chain(
            metadata
                .directors
                .iter()
                .map(|name| (name.as_str(), None, "Director", None, None)),
        )
        .chain(
            metadata
                .creators
                .iter()
                .map(|name| (name.as_str(), None, "Writer", None, None)),
        )
        .filter_map(|(name, role, person_type, tmdb_id, thumb)| {
            let key = tmdb_id
                .map(|id| format!("tmdb:{id}:{person_type}"))
                .unwrap_or_else(|| format!("name:{}:{person_type}", name.to_lowercase()));
            if name.trim().is_empty() || !seen.insert(key.clone()) {
                return None;
            }
            Some(MediaPerson {
                id: uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, key.as_bytes()).to_string(),
                name: name.to_string(),
                role: role.map(str::to_string),
                person_type: person_type.into(),
                tmdb_id: tmdb_id.map(str::to_string),
                primary_image_url: thumb.and_then(person_thumb_url),
            })
        })
        .collect::<Vec<_>>();
    people.shrink_to_fit();
    people
}

pub(in crate::media_server_provider) fn person_image_url(
    row: &domain::LedgerRow,
    media: &Media,
    name: &str,
) -> Option<String> {
    row_nfo_metadata(row, media, false)?
        .cast
        .into_iter()
        .find(|member| member.name.eq_ignore_ascii_case(name))
        .and_then(|member| member.thumb)
        .and_then(|thumb| person_thumb_url(&thumb))
}

pub(in crate::media_server_provider) fn person_tmdb_id(
    row: &domain::LedgerRow,
    media: &Media,
    name: &str,
) -> Option<Option<String>> {
    row_nfo_metadata(row, media, false)?
        .cast
        .into_iter()
        .find(|member| member.name.eq_ignore_ascii_case(name))
        .map(|member| member.tmdb_id)
}

pub(in crate::media_server_provider) fn catalog_person_image_url(
    metadata: &media::ItemMeta,
    name: &str,
    tmdb_person_id: Option<&str>,
) -> Option<String> {
    metadata
        .cast
        .iter()
        .find(|member| match tmdb_person_id {
            Some(person_id) => member.person_id.is_some_and(|id| {
                id.to_string() == person_id
                    || catalog_person_id(id)
                        .replace('-', "")
                        .eq_ignore_ascii_case(&person_id.replace('-', ""))
            }),
            None => member.name.eq_ignore_ascii_case(name),
        })
        .and_then(|member| member.avatar_path.as_deref())
        .and_then(person_thumb_url)
}

fn catalog_person_id(tmdb_id: i64) -> String {
    let key = format!("tmdb:{tmdb_id}:Actor");
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, key.as_bytes()).to_string()
}

fn person_thumb_url(thumb: &str) -> Option<String> {
    if thumb.starts_with("https://image.tmdb.org/") {
        Some(thumb.to_string())
    } else if thumb.starts_with("/t/p/") {
        Some(format!("https://image.tmdb.org{thumb}"))
    } else if thumb
        .strip_prefix('/')
        .is_some_and(|path| !path.is_empty() && !path.contains('/'))
    {
        Some(format!("https://image.tmdb.org/t/p/w185{thumb}"))
    } else {
        None
    }
}
