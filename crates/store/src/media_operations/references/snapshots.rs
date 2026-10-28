//! Source-snapshot composition half of a Media merge: which provider snapshot wins per
//! `(scope, season, episode, source, locale)`, the pending target fallback, and the
//! effective-row recompose that serves the survivor.

use domain::{EntityScope, MediaId};
use rusqlite::{Connection, params};
use serde_json::{Map, Value};

use crate::StoreError;

type MetadataKey = (String, i64, i64, String);

/// The entity/locale keys the loser holds source snapshots or overrides for: only these can
/// change the survivor's composition, so only these are recomposed.
pub(super) fn loser_metadata_keys(
    tx: &Connection,
    loser: &str,
) -> Result<Vec<MetadataKey>, StoreError> {
    let mut stmt = tx.prepare(
        "SELECT entity_scope, season, episode, locale FROM media_metadata_sources WHERE media_id = ?1
         UNION
         SELECT entity_scope, season, episode, locale FROM media_metadata_overrides WHERE media_id = ?1",
    )?;
    let keys = stmt
        .query_map(params![loser], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(keys)
}

fn scope_of(scope: &str, season: i64, episode: i64) -> Result<EntityScope, StoreError> {
    let coordinate = |value: i64| {
        u32::try_from(value)
            .map_err(|_| StoreError::Invalid(format!("metadata coordinate {value} out of range")))
    };
    match scope {
        "work" => Ok(EntityScope::Work),
        "season" => Ok(EntityScope::Season(coordinate(season)?)),
        "episode" => Ok(EntityScope::Episode {
            season: coordinate(season)?,
            episode: coordinate(episode)?,
        }),
        other => Err(StoreError::Invalid(format!("metadata scope {other}"))),
    }
}

/// After the entity/locale decisions are fixed (selected snapshots, merged overrides), the
/// survivor's effective rows for every affected key are recomposed by the same
/// `write_effective` adoption uses. A key with neither snapshot nor override keeps the
/// pending fallback row folded above instead of being blanked.
pub(super) fn recompose_selected(
    tx: &Connection,
    survivor: MediaId,
    keys: &[MetadataKey],
) -> Result<(), StoreError> {
    let media = survivor.to_string();
    for (scope, season, episode, locale) in keys {
        let has_input: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM media_metadata_sources
                           WHERE media_id = ?1 AND entity_scope = ?2 AND season = ?3
                             AND episode = ?4 AND locale = ?5)
                 OR EXISTS(SELECT 1 FROM media_metadata_overrides
                           WHERE media_id = ?1 AND entity_scope = ?2 AND season = ?3
                             AND episode = ?4 AND locale = ?5)",
            params![media, scope, season, episode, locale],
            |r| r.get(0),
        )?;
        if has_input {
            crate::metadata::effective::write_effective(
                tx,
                survivor,
                scope_of(scope, *season, *episode)?,
                locale,
            )?;
        }
    }
    Ok(())
}

/// One `media_metadata_sources` row as the merge needs it.
struct SnapshotRow {
    scope: String,
    season: i64,
    episode: i64,
    source: String,
    locale: String,
    entity: String,
    work: String,
    fetched_at: i64,
    fields_json: String,
}

fn load_snapshot_rows(tx: &Connection, media: &str) -> Result<Vec<SnapshotRow>, StoreError> {
    let mut stmt = tx.prepare(
        "SELECT entity_scope, season, episode, source, locale, provider_entity_id,
                provider_work_id, fetched_at, normalized_json
         FROM media_metadata_sources WHERE media_id = ?1",
    )?;
    let rows = stmt
        .query_map(params![media], |r| {
            Ok(SnapshotRow {
                scope: r.get(0)?,
                season: r.get(1)?,
                episode: r.get(2)?,
                source: r.get(3)?,
                locale: r.get(4)?,
                entity: r.get(5)?,
                work: r.get(6)?,
                fetched_at: r.get(7)?,
                fields_json: r.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The survivor's refresh target per source. `move_aliases` already ran, so the
/// request's choice is applied and there is at most one per `(source, kind)`.
pub(in crate::media_operations) fn survivor_refresh_targets(
    tx: &Connection,
    survivor: &str,
) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
    let mut stmt = tx.prepare(
        "SELECT source, external_id FROM media_aliases
         WHERE media_id = ?1 AND is_refresh_target = 1 ORDER BY source, kind",
    )?;
    let rows = stmt
        .query_map(params![survivor], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.into_iter().collect())
}

const SNAPSHOT_KEY: &str =
    "entity_scope = ?2 AND season = ?3 AND episode = ?4 AND source = ?5 AND locale = ?6";

fn write_pending(
    tx: &Connection,
    media: &str,
    row: &SnapshotRow,
    pending: &crate::metadata::compose::PendingTarget,
) -> Result<(), StoreError> {
    let raw: String = tx.query_row(
        &format!("SELECT selection_origins_json FROM media_metadata_sources WHERE media_id = ?1 AND {SNAPSHOT_KEY}"),
        params![media, row.scope, row.season, row.episode, row.source, row.locale],
        |r| r.get(0),
    )?;
    let mut map = json_object(&raw)?;
    map.insert(
        crate::metadata::compose::PENDING_TARGET_KEY.to_string(),
        serde_json::to_value(pending)?,
    );
    tx.execute(
        &format!("UPDATE media_metadata_sources SET selection_origins_json = ?7 WHERE media_id = ?1 AND {SNAPSHOT_KEY}"),
        params![
            media,
            row.scope,
            row.season,
            row.episode,
            row.source,
            row.locale,
            serde_json::to_string(&Value::Object(map))?
        ],
    )?;
    Ok(())
}

/// Resolve source snapshots per `(scope, season, episode, source, locale)` key (R21).
///
/// * Same provider entity: the newer fetch wins as a whole snapshot; a tie keeps
///   the survivor's.
/// * Different entities and the source has a refresh target: the snapshot whose
///   `provider_work_id` is the target wins at every scope (work, season, episode);
///   the superseded snapshot's fields the target does not carry are kept per field
///   as a *pending target fallback*.
/// * Otherwise the survivor's row wins on a shared key.
///
/// Afterwards any surviving snapshot of a source with a refresh target that does
/// not describe that target is marked whole as pending fallback, so compose serves
/// it with `pending_target_fallback` until the target is adopted for that key.
pub(super) fn merge_source_snapshots(
    tx: &Connection,
    survivor: &str,
    loser: &str,
) -> Result<(), StoreError> {
    let targets = survivor_refresh_targets(tx, survivor)?;
    let survivor_rows = load_snapshot_rows(tx, survivor)?;
    for l in load_snapshot_rows(tx, loser)? {
        let key = (&l.scope, l.season, l.episode, &l.source, &l.locale);
        let s = survivor_rows
            .iter()
            .find(|s| (&s.scope, s.season, s.episode, &s.source, &s.locale) == key);
        let Some(s) = s else {
            tx.execute(
                &format!("UPDATE media_metadata_sources SET media_id = ?7 WHERE media_id = ?1 AND {SNAPSHOT_KEY}"),
                params![loser, l.scope, l.season, l.episode, l.source, l.locale, survivor],
            )?;
            continue;
        };
        let target = targets.get(&l.source);
        let loser_wins = if s.entity == l.entity {
            l.fetched_at > s.fetched_at
        } else {
            target.is_some_and(|t| l.work == *t && s.work != *t)
        };
        let (winner, superseded) = if loser_wins { (&l, s) } else { (s, &l) };
        if loser_wins {
            tx.execute(
                &format!(
                    "DELETE FROM media_metadata_sources WHERE media_id = ?1 AND {SNAPSHOT_KEY}"
                ),
                params![survivor, s.scope, s.season, s.episode, s.source, s.locale],
            )?;
            tx.execute(
                &format!("UPDATE media_metadata_sources SET media_id = ?7 WHERE media_id = ?1 AND {SNAPSHOT_KEY}"),
                params![loser, l.scope, l.season, l.episode, l.source, l.locale, survivor],
            )?;
        } else {
            tx.execute(
                &format!(
                    "DELETE FROM media_metadata_sources WHERE media_id = ?1 AND {SNAPSHOT_KEY}"
                ),
                params![loser, l.scope, l.season, l.episode, l.source, l.locale],
            )?;
        }
        let Some(target) = target.filter(|t| winner.work == **t && superseded.work != **t) else {
            continue;
        };
        let winner_fields: domain::MetadataFields = serde_json::from_str(&winner.fields_json)?;
        let other_fields: domain::MetadataFields = serde_json::from_str(&superseded.fields_json)?;
        if let Some(fields) = crate::metadata::compose::missing_fill(&winner_fields, &other_fields)
        {
            let pending = crate::metadata::compose::PendingTarget {
                target_external_id: target.clone(),
                whole: false,
                fields,
                from_entity_id: superseded.entity.clone(),
            };
            write_pending(tx, survivor, winner, &pending)?;
        }
    }
    for row in load_snapshot_rows(tx, survivor)? {
        let Some(target) = targets.get(&row.source) else {
            continue;
        };
        if row.work.is_empty() || row.work == *target {
            continue;
        }
        let pending = crate::metadata::compose::PendingTarget {
            target_external_id: target.clone(),
            whole: true,
            fields: domain::MetadataFields::default(),
            from_entity_id: row.entity.clone(),
        };
        write_pending(tx, survivor, &row, &pending)?;
    }
    Ok(())
}

pub(super) fn json_object(raw: &str) -> Result<Map<String, Value>, StoreError> {
    match serde_json::from_str::<Value>(raw)? {
        Value::Object(map) => Ok(map),
        _ => Ok(Map::new()),
    }
}
