//! Home selection owns only returned-page files; filesystem work follows lease release.
use super::UpNextCandidate;
use domain::{LibraryId, MediaId, ProtocolItemRef, TypedItemId, UserId};
use std::collections::{HashMap, HashSet};
use store::{HomeQuery, HomeWindow, PageSnapshot, PageValidation, Store, StoreError};

pub(super) struct Captured {
    pub(super) candidates: Vec<UpNextCandidate>,
    pub(super) snapshot: PageSnapshot,
}

pub(super) fn capture(store: &Store, user: UserId, limit: usize) -> Result<Captured, StoreError> {
    let libraries = eligible_libraries(store, user)?;
    let mut entries = store.home_activity_page(&HomeQuery {
        user_id: user,
        libraries: libraries.clone(),
        limit,
    })?;
    let excluded = entries
        .iter()
        .map(|row| row.reference.item.media_id())
        .collect::<Vec<_>>();
    let windows = store
        .list_subscribes_for_user(user)?
        .into_iter()
        .filter_map(|row| match row.coverage {
            domain::Coverage::Tv {
                season,
                episode_from,
                ..
            } => Some(HomeWindow {
                media_id: row.media_id,
                season: season as i32,
                episode_from: episode_from as i32,
                episode_to: None,
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    entries.extend(store.home_subscription_page(
        user,
        &libraries,
        &windows,
        &excluded,
        limit.saturating_sub(entries.len()),
    )?);
    entries.truncate(limit);
    let media = entries
        .iter()
        .map(|row| row.reference.item.media_id())
        .collect::<Vec<MediaId>>();
    let artwork = store.home_preferred_artwork_refs(user, &libraries, &media)?;
    let mut references = entries
        .iter()
        .map(|row| row.reference.clone())
        .collect::<Vec<_>>();
    references.extend(artwork.iter().cloned());
    let inputs = store.protocol_page_inputs(user, &references)?;
    let snapshot = inputs.page_snapshot();
    let candidates = materialize(entries, artwork, inputs)?;
    Ok(Captured {
        candidates,
        snapshot,
    })
}

fn materialize(
    entries: Vec<store::HomeEntry>,
    artwork: Vec<ProtocolItemRef>,
    mut inputs: store::library_query::page_inputs::ProtocolPageInputs,
) -> Result<Vec<UpNextCandidate>, StoreError> {
    let coverage = inputs
        .coverage
        .drain(..)
        .map(|row| (row.file.file_id, row))
        .collect::<HashMap<_, _>>();
    let artwork = artwork
        .into_iter()
        .map(|row| (row.item.media_id(), row))
        .collect::<HashMap<_, _>>();
    let mut candidates = Vec::with_capacity(entries.len());
    for entry in entries {
        let reference = entry.reference;
        let media_id = reference.item.media_id();
        let file = source_id(&reference)?;
        let selected = coverage
            .get(&file)
            .cloned()
            .ok_or_else(|| StoreError::Invalid("home selected coverage missing".into()))?;
        let media = inputs
            .media
            .remove(&media_id)
            .ok_or_else(|| StoreError::Invalid("home selected Media missing".into()))?;
        let key = match reference.item {
            TypedItemId::Unit { key, .. } => key,
            _ => domain::UnitKey::WHOLE,
        };
        let unit = inputs.user_items.iter().find(|row| {
            row.media_id == media_id && row.season == key.season && row.episode == key.episode
        });
        let position = unit.map(|row| row.state.position_ms).unwrap_or(0);
        let duration = unit.and_then(|row| row.state.duration_ms);
        let progress = duration
            .filter(|duration| *duration > 0)
            .map(|duration| ((position as f64 / duration as f64) * 100.).round() as i64)
            .filter(|progress| (0..=100).contains(progress));
        let preferred_coverage = artwork
            .get(&media_id)
            .and_then(|row| row.preferred_source.as_ref())
            .and_then(|source| coverage.get(&source.file_id))
            .cloned()
            .or_else(|| Some(selected.clone()));
        candidates.push(UpNextCandidate {
            media,
            library_id: reference.library_id.to_string(),
            preferred_coverage,
            next_row: Some(selected),
            plan_season: key.season.max(0) as u32,
            plan_next_episode: key.episode.max(0),
            episode_title: None,
            position,
            duration,
            progress,
            advanced: entry.advanced,
            last_played_at: if entry.last_activity > 0 {
                super::millis(entry.last_activity)
            } else {
                String::new()
            },
        });
    }
    Ok(candidates)
}

fn source_id(reference: &ProtocolItemRef) -> Result<domain::LedgerId, StoreError> {
    reference
        .preferred_source
        .as_ref()
        .map(|source| source.file_id)
        .ok_or_else(|| StoreError::Invalid("home selected source missing".into()))
}

fn eligible_libraries(store: &Store, user: UserId) -> Result<Vec<LibraryId>, StoreError> {
    let allowed: HashSet<_> = store.authorized_library_ids(user)?.into_iter().collect();
    store
        .list_libraries()?
        .into_iter()
        .filter(|row| !row.exclude_from_home && allowed.contains(&row.id))
        .map(|row| row.id.parse().map_err(Into::into))
        .collect()
}

pub(super) fn validate(
    store: &Store,
    user: UserId,
    snapshot: &PageSnapshot,
) -> Result<PageValidation, StoreError> {
    let eligible: HashSet<_> = eligible_libraries(store, user)?.into_iter().collect();
    let mut gone: HashSet<_> = snapshot
        .files
        .iter()
        .filter(|row| !eligible.contains(&row.3))
        .map(|row| row.0)
        .collect();
    match store.validate_page_snapshot(snapshot)? {
        PageValidation::Fresh if gone.is_empty() => Ok(PageValidation::Fresh),
        PageValidation::Fresh => Ok(PageValidation::Stale {
            gone: gone.into_iter().collect(),
        }),
        PageValidation::Stale { gone: changed } => {
            gone.extend(changed);
            Ok(PageValidation::Stale {
                gone: gone.into_iter().collect(),
            })
        }
    }
}
