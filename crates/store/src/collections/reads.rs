//! Batched, visibility-scoped collection read facts.
use std::collections::HashMap;

use domain::{FileCoverage, LibraryId, Media, UserId};
use rusqlite::{Connection, params, types::Value};

use super::{
    UserCollection, cursor,
    facts::{self, Member},
};
use crate::{Store, StoreError};

#[derive(Clone, Debug)]
pub struct CollectionItemFacts {
    pub media: Media,
    pub library_id: LibraryId,
    pub coverages: Vec<FileCoverage>,
    pub is_favorite: bool,
}

pub struct CollectionItemPage {
    pub items: Vec<CollectionItemFacts>,
    pub total: usize,
    pub next_cursor: Option<String>,
}

pub struct CollectionRead {
    pub collection: UserCollection,
    pub items: Vec<CollectionItemFacts>,
}

const VISIBLE: &str = "EXISTS(SELECT 1 FROM library_files f WHERE f.media_id=ci.media_id AND f.library_id IN (SELECT value FROM json_each(?2)))";
const OWNER: &str = "(SELECT MIN(f.library_id) FROM library_files f WHERE f.media_id=ci.media_id AND f.library_id IN (SELECT value FROM json_each(?2)))";

fn own(conn: &Connection, user: UserId, collection: &str) -> Result<(), StoreError> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM collections WHERE id=?1 AND user_id=?2)",
        params![collection, user.to_string()],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(StoreError::Missing("collection".into()))
    }
}

fn scope_json(libraries: &[LibraryId]) -> Result<String, StoreError> {
    Ok(serde_json::to_string(
        &libraries
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
    )?)
}

/// Defend against a caller retaining an authorization snapshot during removal.
fn visible_scope(conn: &Connection, requested: &str) -> Result<String, StoreError> {
    Ok(conn.query_row(
        "SELECT json_group_array(l.library_id) FROM libraries l
         WHERE l.library_id IN (SELECT value FROM json_each(?1))
         AND l.lifecycle_state='ready' AND l.integrity_state<>'failed'
         AND NOT EXISTS(SELECT 1 FROM library_removals r WHERE r.library_id=l.library_id)",
        [requested],
        |row| row.get(0),
    )?)
}

fn member_query(collection: bool, after: bool) -> String {
    let seek = if after {
        "AND (ci.sort_order,ci.added_at,ci.media_id)>(?4,?5,?6)"
    } else {
        ""
    };
    if collection {
        format!(
            "SELECT ci.collection_id,ci.media_id,{OWNER},ci.sort_order,ci.added_at
             FROM collection_items ci WHERE ci.collection_id=?3 AND {VISIBLE} {seek}
             ORDER BY ci.sort_order,ci.added_at,ci.media_id LIMIT ?7"
        )
    } else {
        format!(
            "SELECT ci.collection_id,ci.media_id,{OWNER},ci.sort_order,ci.added_at
             FROM collections c JOIN collection_items ci ON ci.collection_id=c.id
             WHERE c.user_id=?1 AND {VISIBLE} {seek}
             ORDER BY c.sort_order,c.id,ci.sort_order,ci.added_at,ci.media_id LIMIT ?7"
        )
    }
}

fn member_values(
    user: UserId,
    collection: Option<&str>,
    libraries: &str,
    after: Option<&cursor::Cursor>,
    limit: Option<usize>,
) -> Result<Vec<Value>, StoreError> {
    let limit = limit
        .map(i64::try_from)
        .transpose()
        .map_err(|_| StoreError::Invalid("collection page limit".into()))?
        .unwrap_or(-1);
    Ok(vec![
        Value::Text(user.to_string()),
        Value::Text(libraries.to_owned()),
        Value::from(collection.map(str::to_owned)),
        Value::from(after.map(|cursor| cursor.order)),
        Value::from(after.map(|cursor| cursor.added.clone())),
        Value::from(after.map(|cursor| cursor.media.clone())),
        Value::Integer(limit),
    ])
}

fn members(
    conn: &Connection,
    user: UserId,
    collection: Option<&str>,
    libraries: &str,
    after: Option<&cursor::Cursor>,
    limit: Option<usize>,
) -> Result<Vec<Member>, StoreError> {
    let sql = member_query(collection.is_some(), after.is_some());
    let values = member_values(user, collection, libraries, after, limit)?;
    let mut statement = conn.prepare_cached(&sql)?;
    let mut rows = statement.query(rusqlite::params_from_iter(values))?;
    let mut members = Vec::new();
    while let Some(row) = rows.next()? {
        members.push(Member {
            collection: row.get(0)?,
            media: row.get::<_, String>(1)?.parse()?,
            library: row.get::<_, String>(2)?.parse()?,
            order: row.get(3)?,
            added: row.get(4)?,
        });
    }
    Ok(members)
}

impl Store {
    #[cfg(any(test, feature = "test-support"))]
    pub fn collection_items_page_query_plans(
        &self,
        user: UserId,
        collection: &str,
        libraries: &[LibraryId],
        limit: usize,
        raw_cursor: Option<&str>,
    ) -> Result<Vec<String>, StoreError> {
        let scope = cursor::scope(user, collection, libraries);
        let after = cursor::decode(raw_cursor, &scope)?;
        let libraries = scope_json(libraries)?;
        let sql = format!("EXPLAIN QUERY PLAN {}", member_query(true, after.is_some()));
        self.with_read(crate::Db::Library, |conn| {
            let libraries = visible_scope(conn, &libraries)?;
            let values = member_values(
                user,
                Some(collection),
                &libraries,
                after.as_ref(),
                Some(limit + 1),
            )?;

            let mut statement = conn.prepare(&sql)?;
            Ok(statement
                .query_map(rusqlite::params_from_iter(values), |row| {
                    row.get::<_, String>(3)
                })?
                .collect::<Result<Vec<_>, _>>()?)
        })
    }

    pub fn collection_items_page(
        &self,
        user: UserId,
        collection: &str,
        libraries: &[LibraryId],
        limit: usize,
        raw_cursor: Option<&str>,
    ) -> Result<CollectionItemPage, StoreError> {
        if !(1..=200).contains(&limit) {
            return Err(StoreError::Invalid(
                "collection limit must be 1..200".into(),
            ));
        }
        let scope = cursor::scope(user, collection, libraries);
        let after = cursor::decode(raw_cursor, &scope)?;
        let libraries = scope_json(libraries)?;
        self.with_read(crate::Db::Library, |conn| {
            let libraries = visible_scope(conn, &libraries)?;
            own(conn, user, collection)?;
            let count = format!(
                "SELECT COUNT(*) FROM collection_items ci WHERE ci.collection_id=?1 AND {VISIBLE}"
            );
            let total: usize =
                conn.query_row(&count, params![collection, libraries], |row| row.get(0))?;
            let mut members = members(
                conn,
                user,
                Some(collection),
                &libraries,
                after.as_ref(),
                Some(limit + 1),
            )?;
            let more = members.len() > limit;
            members.truncate(limit);
            let next_cursor = if more {
                members
                    .last()
                    .map(|member| {
                        cursor::encode(
                            scope,
                            member.order,
                            member.added.clone(),
                            member.media.to_string(),
                        )
                    })
                    .transpose()?
            } else {
                None
            };
            let hydrated = facts::hydrate(conn, user, &members)?;
            Ok(CollectionItemPage {
                items: facts::ordered(members, &hydrated)?,
                total,
                next_cursor,
            })
        })
    }

    pub fn collection_summaries(
        &self,
        user: UserId,
        libraries: &[LibraryId],
    ) -> Result<Vec<CollectionRead>, StoreError> {
        let libraries = scope_json(libraries)?;
        self.with_read(crate::Db::Library, |conn| {
            let libraries = visible_scope(conn, &libraries)?;
            let mut statement = conn.prepare_cached("SELECT id,name,sort_order FROM collections WHERE user_id=?1 ORDER BY sort_order,created_at,id")?;
            let collections = statement.query_map([user.to_string()], |row| Ok(UserCollection {id:row.get(0)?,name:row.get(1)?,sort_order:row.get(2)?}))?.collect::<Result<Vec<_>,_>>()?;
            let members = members(conn, user, None, &libraries, None, None)?;
            let hydrated = facts::hydrate(conn, user, &members)?;
            let mut grouped: HashMap<String,Vec<CollectionItemFacts>> = HashMap::new();
            for member in members {
                let item = hydrated.get(&member.media).cloned().ok_or_else(|| StoreError::Missing("collection Media".into()))?;
                grouped.entry(member.collection).or_default().push(item);
            }
            Ok(collections.into_iter().map(|collection| CollectionRead {items:grouped.remove(&collection.id).unwrap_or_default(),collection}).collect())
        })
    }

    pub fn collection_items_all(
        &self,
        user: UserId,
        collection: &str,
        libraries: &[LibraryId],
    ) -> Result<Vec<CollectionItemFacts>, StoreError> {
        let libraries = scope_json(libraries)?;
        self.with_read(crate::Db::Library, |conn| {
            let libraries = visible_scope(conn, &libraries)?;
            own(conn, user, collection)?;
            let members = members(conn, user, Some(collection), &libraries, None, None)?;
            let hydrated = facts::hydrate(conn, user, &members)?;
            facts::ordered(members, &hydrated)
        })
    }
}
