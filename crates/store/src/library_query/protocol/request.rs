//! Normalize classification keys once without changing search or ordering.
use crate::{
    StoreError,
    protocol_index::{compact_id, requested_genre_keys},
};
use domain::{ProtocolItemQuery, ProtocolParent};
use std::collections::BTreeSet;

pub(super) struct NormalizedProtocolRequest {
    pub(super) original: ProtocolItemQuery,
    pub(super) genres: Vec<String>,
    pub(super) studios: Vec<String>,
    pub(super) studio_wire_ids: Vec<String>,
    pub(super) parent_genres: Vec<String>,
    pub(super) parent_studios: Vec<String>,
    pub(super) parent_studio_wire_ids: Vec<String>,
    pub(super) people: Vec<String>,
}

pub(super) fn normalize(
    request: &ProtocolItemQuery,
) -> Result<NormalizedProtocolRequest, StoreError> {
    let mut genres = request.genres.clone();
    genres.extend(request.genre_ids.iter().cloned());
    let parent_genres = match &request.parent {
        ProtocolParent::Genre(value) => requested_genre_keys(value),
        _ => Vec::new(),
    };
    let parent_studios = match &request.parent {
        ProtocolParent::Studio(value) => vec![value.clone()],
        _ => Vec::new(),
    };
    Ok(NormalizedProtocolRequest {
        original: request.clone(),
        genres: genres
            .iter()
            .flat_map(|v| requested_genre_keys(v))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        studios: keys(&request.studio_ids, |v| v.to_lowercase()),
        studio_wire_ids: studio_ids(&request.studio_ids),
        parent_genres,
        parent_studios: keys(&parent_studios, |v| v.to_lowercase()),
        parent_studio_wire_ids: studio_ids(&parent_studios),
        people: keys(&request.person_ids, compact_id),
    })
}

fn studio_ids(values: &[String]) -> Vec<String> {
    values
        .iter()
        .filter_map(|value| uuid::Uuid::parse_str(value.trim()).ok())
        .map(|id| id.simple().to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn keys(values: &[String], map: impl Fn(&str) -> String) -> Vec<String> {
    values
        .iter()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .map(map)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
