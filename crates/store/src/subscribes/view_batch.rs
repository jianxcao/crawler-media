//! Bounded inputs for Subscribe list rendering. This DTO carries persisted slot
//! facts; list progress never consumes the round selector's per-path Release map.
use super::{PendingDownload, release_from_json, torrent_from_json};
use crate::{Store, StoreError};
use domain::{SubscribeFacts, SubscribeId};
use rusqlite::{Connection, params_from_iter};
use std::collections::HashMap;

pub struct SubscribeViewInput {
    pub facts: SubscribeFacts,
    pub pending: Vec<(i32, PendingDownload)>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}
impl Store {
    pub fn subscribe_view_inputs(
        &self,
        ids: &[SubscribeId],
    ) -> Result<HashMap<SubscribeId, SubscribeViewInput>, StoreError> {
        self.with_read(crate::Db::Subscribe, |conn| {
            let mut out = HashMap::new();
            for chunk in ids.chunks(400) {
                load_chunk(conn, chunk, &mut out)?;
            }
            Ok(out)
        })
    }
}
fn load_chunk(
    conn: &Connection,
    ids: &[SubscribeId],
    out: &mut HashMap<SubscribeId, SubscribeViewInput>,
) -> Result<(), StoreError> {
    let keys: Vec<_> = ids.iter().map(ToString::to_string).collect();
    let placeholders = vec!["?"; ids.len()].join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT id,created_at,updated_at FROM subscribes WHERE id IN ({placeholders})"
    ))?;
    for row in stmt.query_map(params_from_iter(&keys), |r| {
        Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?))
    })? {
        let (id, created_at_ms, updated_at_ms) = row?;
        out.insert(
            id.parse()?,
            SubscribeViewInput {
                facts: SubscribeFacts::default(),
                pending: Vec::new(),
                created_at_ms,
                updated_at_ms,
            },
        );
    }
    let mut stmt = conn.prepare(&format!("SELECT subscribe_id,season,episode,score,path FROM subscribe_facts WHERE subscribe_id IN ({placeholders}) AND path IS NOT NULL"))?;
    for row in stmt.query_map(params_from_iter(&keys), |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get(3)?,
            r.get(4)?,
        ))
    })? {
        let (id, season, episode, score, path) = row?;
        let id: SubscribeId = id.parse()?;
        let input = out
            .get_mut(&id)
            .ok_or_else(|| StoreError::Missing(format!("Subscribe {id}")))?;
        input.facts.upsert(
            (season >= 0).then_some(season as u32),
            (episode >= 0).then_some(episode as u32),
            domain::QualityFact { score, path },
        );
    }
    let mut stmt = conn.prepare(&format!("SELECT subscribe_id,score,torrent_json,submitted_at FROM pending_downloads WHERE subscribe_id IN ({placeholders}) AND state='active'"))?;
    for row in stmt.query_map(params_from_iter(&keys), |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })? {
        let (id, score, raw, submitted_at) = row?;
        let id: SubscribeId = id.parse()?;
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        let pending = PendingDownload {
            torrent: torrent_from_json(&value)?,
            release_override: release_from_json(value.get("release_override")),
            downloader_id: value
                .get("downloader_id")
                .and_then(|v| v.as_str())
                .map(str::parse)
                .transpose()?,
            submitted_at: (submitted_at > 0).then_some(submitted_at),
        };
        out.get_mut(&id)
            .ok_or_else(|| StoreError::Missing(format!("Subscribe {id}")))?
            .pending
            .push((score, pending));
    }
    for id in ids {
        if !out.contains_key(id) {
            return Err(StoreError::Missing(format!("Subscribe {id} timestamps")));
        }
    }
    Ok(())
}
