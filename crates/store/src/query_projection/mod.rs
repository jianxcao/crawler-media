//! Atomic query membership and distinct per-Library counter settlement.
mod browse;
mod counts;
pub(crate) mod expected;
mod people;
mod protocol;
mod repair;
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
    let members = match &kind {
        Some(kind) => counts::members(
            kind,
            &browse::read_terms(tx, "library_item_terms", &lib, &media)?,
            &browse::read_terms(tx, "library_protocol_terms", &lib, &media)?,
        ),
        None => Default::default(),
    };
    Ok(ProjectionBefore {
        card_exists: kind.is_some(),
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
    let counters = counts::apply(
        tx,
        &lib,
        &before.members,
        &counts::members(&kind, &web, &protocol),
    )?;
    mark_current(tx, &lib, &media)?;
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
    let title: String = tx.query_row(
        "SELECT title FROM library_items WHERE library_id=?1 AND media_id=?2",
        params![lib, media],
        |r| r.get(0),
    )?;
    tx.execute("UPDATE library_items SET protocol_title_key=?3,query_projection_revision=item_revision,query_config_revision=(SELECT revision FROM library_query_config WHERE id=1) WHERE library_id=?1 AND media_id=?2",params![lib,media,title.to_lowercase()])?;
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
