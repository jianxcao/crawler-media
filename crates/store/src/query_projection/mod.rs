//! Atomic query membership and distinct per-Library counter settlement.
mod browse;
mod continuation;
mod counts;
mod entities;
pub(crate) mod entity_integrity;
pub(crate) mod expected;
mod people;
mod protocol;
mod repair;
pub(crate) mod search;
pub(crate) mod search_integrity;
mod types;
use crate::StoreError;
pub(crate) use browse::sync_terms;
pub(crate) use expected::{ExpectedProjection, expected_projection};
pub(crate) use repair::{projection_matches, repair_projection};
use rusqlite::{Connection, OptionalExtension, params};
pub(crate) use types::{ProjectionBefore, ProjectionDelta, ProjectionKey};

pub(crate) fn capture(tx: &Connection, key: ProjectionKey) -> Result<ProjectionBefore, StoreError> {
    let (lib, media) = (key.library_id.to_string(), key.media_id.to_string());
    let kind: Option<String> = tx
        .query_row(
            "SELECT kind FROM library_items WHERE library_id=?1 AND media_id=?2",
            params![lib, media],
            |r| r.get(0),
        )
        .optional()?;
    let mut members = match &kind {
        Some(kind) => counts::members(
            kind,
            &browse::read_terms(tx, "library_item_terms", &lib, &media)?,
            &browse::read_terms(tx, "library_protocol_terms", &lib, &media)?,
        ),
        None => Default::default(),
    };
    members.extend(entities::members(&entities::read(tx, &lib, &media)?));
    Ok(ProjectionBefore {
        entities: entities::ownership(tx, &media)?,
        card_exists: kind.is_some(),
        search_rowid: search::rowid(tx, key)?,
        members,
    })
}
pub(crate) fn settle(
    tx: &Connection,
    key: ProjectionKey,
    before: ProjectionBefore,
) -> Result<ProjectionDelta, StoreError> {
    let (lib, media) = (key.library_id.to_string(), key.media_id.to_string());
    let kind: Option<String> = tx
        .query_row(
            "SELECT kind FROM library_items WHERE library_id=?1 AND media_id=?2",
            params![lib, media],
            |r| r.get(0),
        )
        .optional()?;
    let Some(kind) = kind else {
        return remove(tx, key, before);
    };
    let old = browse::read_terms(tx, "library_item_terms", &lib, &media)?;
    let mut web = old.clone();
    web.retain(|(kind, _)| {
        !matches!(
            kind.as_str(),
            "decade" | "runtime" | "language" | "rating" | "hdr" | "stock"
        )
    });
    web.extend(browse::from_card(tx, &lib, &media)?);
    let (mut inserted, mut deleted) =
        browse::sync_terms(tx, "library_item_terms", &lib, &media, &old, &web)?;
    let (protocol, people) = protocol::derive(tx, &lib, &media, &web)?;
    let old = browse::read_terms(tx, "library_protocol_terms", &lib, &media)?;
    let delta = browse::sync_terms(tx, "library_protocol_terms", &lib, &media, &old, &protocol)?;
    inserted += delta.0;
    deleted += delta.1;
    people::sync(tx, &lib, &media, &people)?;
    let entities = entities::derive(tx, &lib, &media, &web)?;
    entities::sync(tx, &lib, &media, &entities, &before.entities)?;
    let mut after_members = counts::members(&kind, &web, &protocol);
    after_members.extend(entities::members(&entities));
    let counters = counts::apply(tx, &lib, &before.members, &after_members)?;
    mark_current(tx, &lib, &media)?;
    search::sync(tx, key, &people)?;
    tracing::debug!(library_id=%key.library_id,media_id=%key.media_id,inserted,deleted,counters,"settled query projection");
    Ok(ProjectionDelta {
        cards: 1,
        inserted,
        deleted,
        counters,
    })
}
pub(crate) fn remove(
    tx: &Connection,
    key: ProjectionKey,
    before: ProjectionBefore,
) -> Result<ProjectionDelta, StoreError> {
    search::remove(tx, before.search_rowid)?;
    entities::sync(
        tx,
        &key.library_id.to_string(),
        &key.media_id.to_string(),
        &Default::default(),
        &before.entities,
    )?;
    let counters = counts::apply(
        tx,
        &key.library_id.to_string(),
        &before.members,
        &Default::default(),
    )?;
    Ok(ProjectionDelta {
        cards: u64::from(before.card_exists),
        counters,
        ..Default::default()
    })
}
fn mark_current(tx: &Connection, lib: &str, media: &str) -> Result<(), StoreError> {
    continuation::refresh_default_unit(tx, lib, media)?;
    let title: String = tx.query_row(
        "SELECT title FROM library_items WHERE library_id=?1 AND media_id=?2",
        params![lib, media],
        |r| r.get(0),
    )?;
    let original = protocol::original_title(tx, lib, media)?;
    tx.execute("UPDATE library_items SET protocol_title_key=?3,protocol_original_title_key=?4,query_projection_revision=item_revision,query_config_revision=(SELECT revision FROM library_query_config WHERE id=1) WHERE library_id=?1 AND media_id=?2",params![lib,media,title.to_lowercase(),original])?;
    Ok(())
}

pub(crate) fn base_terms(
    tx: &Connection,
    lib: &str,
    media: &str,
) -> Result<std::collections::BTreeSet<(String, String)>, StoreError> {
    Ok(browse::read_terms(tx, "library_item_terms", lib, media)?
        .into_iter()
        .filter(|(k, _)| {
            matches!(
                k.as_str(),
                "genre" | "country" | "resolution" | "collection"
            )
        })
        .collect())
}
