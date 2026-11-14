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
    pub(super) people: Vec<String>,
}

pub(super) fn normalize(
    request: &ProtocolItemQuery,
) -> Result<NormalizedProtocolRequest, StoreError> {
    let mut genres = request.genres.clone();
    genres.extend(request.genre_ids.iter().cloned());
    if let ProtocolParent::Genre(name) = &request.parent {
        genres.push(name.clone());
    }
    let mut studios = request.studio_ids.clone();
    if let ProtocolParent::Studio(name) = &request.parent {
        studios.push(name.clone());
    }
    Ok(NormalizedProtocolRequest {
        original: request.clone(),
        genres: genres
            .iter()
            .flat_map(|v| requested_genre_keys(v))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        studios: keys(&studios, |v| v.to_lowercase()),
        people: keys(&request.person_ids, compact_id),
    })
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
