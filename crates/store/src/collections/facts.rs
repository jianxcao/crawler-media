//! Hydrate selected logical members in fixed SQL batches on the leased snapshot.
use std::collections::{HashMap, HashSet};

use domain::{LibraryId, MediaId, UserId};
use rusqlite::Connection;

use super::reads::CollectionItemFacts;
use crate::{
    StoreError,
    maps::{LIBRARY_FILE_COLUMNS, group_file_coverage, map_file_unit, map_media},
};

pub(super) struct Member {
    pub collection: String,
    pub media: MediaId,
    pub library: LibraryId,
    pub order: i64,
    pub added: String,
}

pub(super) fn hydrate(
    conn: &Connection,
    user: UserId,
    members: &[Member],
) -> Result<HashMap<MediaId, CollectionItemFacts>, StoreError> {
    let ids: HashSet<_> = members
        .iter()
        .map(|member| member.media.to_string())
        .collect();
    let ids = serde_json::to_string(&ids)?;
    let media_sql = format!(
        "SELECT m.*,COALESCE(p.favorite,0) FROM ({}) m
         LEFT JOIN playback_units p ON p.user_id=?2 AND p.media_id=m.media_id
         AND p.season=-1 AND p.episode=-1 WHERE m.media_id IN (SELECT value FROM json_each(?1))",
        crate::media::MEDIA_SELECT
    );
    let mut statement = conn.prepare_cached(&media_sql)?;
    let mut rows = statement.query(rusqlite::params![ids, user.to_string()])?;
    let owners: HashMap<_, _> = members
        .iter()
        .map(|member| (member.media, member.library))
        .collect();
    let mut facts = HashMap::new();
    while let Some(row) = rows.next()? {
        let media = map_media(row)?;
        if let Some(library_id) = owners.get(&media.id) {
            facts.insert(
                media.id,
                CollectionItemFacts {
                    media,
                    library_id: *library_id,
                    coverages: Vec::new(),
                    is_favorite: row.get::<_, i64>(11)? != 0,
                },
            );
        }
    }
    let pairs: HashSet<_> = members
        .iter()
        .map(|member| (member.media.to_string(), member.library.to_string()))
        .collect();
    let pairs = serde_json::to_string(&pairs)?;
    let sql = format!(
        "SELECT {LIBRARY_FILE_COLUMNS},u.season,u.episode
         FROM json_each(?1) keys CROSS JOIN library_files f
           ON f.media_id=json_extract(keys.value,'$[0]') AND f.library_id=json_extract(keys.value,'$[1]')
         LEFT JOIN library_file_units u ON u.file_id=f.file_id"
    );
    let mut statement = conn.prepare_cached(&sql)?;
    let rows = statement
        .query_map([pairs], map_file_unit)?
        .collect::<Result<Vec<_>, _>>()?;
    for coverage in group_file_coverage(rows) {
        if let Some(fact) = facts.get_mut(&coverage.file.media_id) {
            fact.coverages.push(coverage);
        }
    }
    Ok(facts)
}

pub(super) fn ordered(
    members: Vec<Member>,
    facts: &HashMap<MediaId, CollectionItemFacts>,
) -> Result<Vec<CollectionItemFacts>, StoreError> {
    members
        .into_iter()
        .map(|member| {
            facts
                .get(&member.media)
                .cloned()
                .ok_or_else(|| StoreError::Missing(format!("collection Media {}", member.media)))
        })
        .collect()
}
