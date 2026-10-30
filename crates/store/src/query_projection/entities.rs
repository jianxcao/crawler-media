//! Display entities and exact owner-set overlap counters, settled with each card.
use super::types::{CounterMember, Terms};
use crate::StoreError;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) type Entities = BTreeSet<(String, String)>;
pub(crate) type Ownership = BTreeMap<(String, String), BTreeSet<String>>;

pub(super) fn derive(
    tx: &Connection,
    lib: &str,
    media: &str,
    _web: &Terms,
) -> Result<Entities, StoreError> {
    let (nfo, adopted, genres) = super::protocol::selected(tx, lib, media)?;
    let mut entities = Entities::new();
    for (kind, values) in [
        ("genre", &genres),
        ("genre", &adopted["fields"]["genres"]["value"]),
        ("genre", &nfo["genres"]["value"]),
        ("studio", &nfo["studios"]["value"]),
        ("studio", &adopted["fields"]["studios"]["value"]),
        ("studio", &adopted["fields"]["studios"]),
    ] {
        for value in values.as_array().into_iter().flatten() {
            if let Some(name) = value
                .as_str()
                .or_else(|| value["name"].as_str())
                .filter(|s| !s.trim().is_empty())
            {
                entities.insert((kind.into(), name.into()));
            }
        }
    }
    Ok(entities)
}
pub(super) fn read(tx: &Connection, lib: &str, media: &str) -> Result<Entities, StoreError> {
    Ok(tx.prepare_cached("SELECT entity_kind,name FROM library_protocol_entities WHERE library_id=?1 AND media_id=?2")?
        .query_map(params![lib,media], |r| Ok((r.get(0)?,r.get(1)?)))?.collect::<Result<_,_>>()?)
}
pub(super) fn ownership(tx: &Connection, media: &str) -> Result<Ownership, StoreError> {
    let mut out = Ownership::new();
    let mut statement = tx.prepare_cached(
        "SELECT entity_kind,name,library_id FROM library_protocol_entities WHERE media_id=?1",
    )?;
    for row in statement.query_map([media], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })? {
        let (kind, name, lib) = row?;
        out.entry((kind, name)).or_default().insert(lib);
    }
    Ok(out)
}
pub(super) fn members(entities: &Entities) -> impl Iterator<Item = CounterMember> + '_ {
    entities
        .iter()
        .map(|(kind, name)| CounterMember::new("protocol_entity", kind, name))
}
pub(super) fn sync(
    tx: &Connection,
    lib: &str,
    media: &str,
    after: &Entities,
    before: &Ownership,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM library_protocol_entities WHERE library_id=?1 AND media_id=?2",
        params![lib, media],
    )?;
    let poster: Option<String> = tx
        .query_row(
            "SELECT poster_ref FROM library_items WHERE library_id=?1 AND media_id=?2",
            params![lib, media],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    for (kind, name) in after {
        let wire = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes())
            .simple()
            .to_string();
        tx.execute("INSERT INTO library_protocol_entities(library_id,media_id,entity_kind,name,name_key,wire_id,poster_file_id) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![lib,media,kind,name,name.to_lowercase(),wire,poster])?;
    }
    let after = ownership(tx, media)?;
    for (key, owners) in before {
        if owners.len() > 1 {
            adjust(tx, key, owners, -1)?;
        }
    }
    for (key, owners) in &after {
        if owners.len() > 1 {
            adjust(tx, key, owners, 1)?;
        }
    }
    Ok(())
}
fn adjust(
    tx: &Connection,
    key: &(String, String),
    owners: &BTreeSet<String>,
    delta: i64,
) -> Result<(), StoreError> {
    let owners = serde_json::to_string(owners)?;
    if delta < 0 {
        tx.execute("DELETE FROM protocol_entity_overlap_counts WHERE entity_kind=?1 AND name=?2 AND owner_libraries_json=?3 AND member_count=1",params![key.0,key.1,owners])?;
        tx.execute("UPDATE protocol_entity_overlap_counts SET member_count=member_count-1 WHERE entity_kind=?1 AND name=?2 AND owner_libraries_json=?3",params![key.0,key.1,owners])?;
    } else {
        tx.execute("INSERT INTO protocol_entity_overlap_counts(entity_kind,name,owner_libraries_json,member_count) VALUES(?1,?2,?3,1) ON CONFLICT(entity_kind,name,owner_libraries_json) DO UPDATE SET member_count=member_count+1",params![key.0,key.1,owners])?;
    }
    Ok(())
}

pub(super) fn sources_match(tx: &Connection, lib: &str, media: &str) -> Result<bool, StoreError> {
    Ok(tx.query_row("SELECT NOT EXISTS(SELECT 1 FROM library_protocol_entities e LEFT JOIN library_items i ON i.library_id=e.library_id AND i.media_id=e.media_id WHERE e.library_id=?1 AND e.media_id=?2 AND e.poster_file_id IS NOT i.poster_ref)",params![lib,media],|r|r.get(0))?)
}
