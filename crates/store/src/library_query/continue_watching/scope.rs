//! Scope predicates are shared by actual continuation queries and diagnostics.
use super::ContinueQuery;
use crate::{Store, StoreError};
use domain::{LibraryId, ProtocolParent, UserId};
use rusqlite::{Connection, types::Value};

pub(super) struct Sql {
    pub values: Vec<Value>,
    pub allowed: String,
    pub cards: String,
    pub season: Option<String>,
    pub root: Option<String>,
}
impl Sql {
    pub fn bind(&mut self, value: impl Into<Value>) -> String {
        self.values.push(value.into());
        format!("?{}", self.values.len())
    }
    pub fn new(user: UserId, libraries: &[LibraryId], home: bool) -> Result<Self, StoreError> {
        let home = if home {
            " AND l.exclude_from_home=0"
        } else {
            ""
        };
        Ok(Self {
            values: vec![
                user.to_string().into(),
                serde_json::to_string(libraries)?.into(),
            ],
            allowed: format!(
                "allowed AS MATERIALIZED (SELECT l.library_id FROM libraries l WHERE l.library_id IN (SELECT value FROM json_each(?2)) AND l.lifecycle_state='ready' AND l.integrity_state<>'failed' {home} AND NOT EXISTS(SELECT 1 FROM library_removals r WHERE r.library_id=l.library_id AND r.state<>'succeeded'))"
            ),
            cards: "i.library_id IN (SELECT library_id FROM allowed) AND i.available_file_count>0"
                .into(),
            season: None,
            root: None,
        })
    }
    pub fn protocol(query: &ContinueQuery, libraries: &[LibraryId]) -> Result<Self, StoreError> {
        let mut sql = Self::new(query.user_id, libraries, false)?;
        match &query.parent {
            ProtocolParent::Root => {}
            ProtocolParent::Library(library) => {
                let id = sql.bind(library.to_string());
                sql.cards.push_str(&format!(" AND i.library_id={id}"));
            }
            ProtocolParent::Series(media) | ProtocolParent::Season(media, _) => {
                let id = sql.bind(media.to_string());
                sql.cards.push_str(&format!(" AND i.media_id={id}"));
            }
            ProtocolParent::Folder { root_id } => sql.root = Some(sql.bind(root_id.clone())),
            ProtocolParent::Genre(name) => {
                let keys = crate::protocol_index::requested_genre_keys(name);
                let keys = sql.bind(serde_json::to_string(&keys)?);
                sql.cards.push_str(&format!(" AND EXISTS(SELECT 1 FROM library_protocol_terms t WHERE t.library_id=i.library_id AND t.media_id=i.media_id AND t.term_kind='genre' AND t.term_value IN (SELECT value FROM json_each({keys})))"));
            }
            ProtocolParent::Studio(name) => {
                let key = sql.bind(name.trim().to_lowercase());
                sql.cards.push_str(&format!(" AND EXISTS(SELECT 1 FROM library_protocol_terms t WHERE t.library_id=i.library_id AND t.media_id=i.media_id AND t.term_kind='studio' AND t.term_value={key})"));
            }
            ProtocolParent::Unknown => sql.cards.push_str(" AND 0"),
        }
        if let Some(media) = query.media_id {
            let id = sql.bind(media.to_string());
            sql.cards.push_str(&format!(" AND i.media_id={id}"));
        }
        let parent_season = match &query.parent {
            ProtocolParent::Season(_, season) => Some(i64::from(*season)),
            _ => None,
        };
        if let Some(season) = parent_season {
            sql.season = Some(sql.bind(season));
        }
        if let Some(season) = query.season {
            if parent_season.is_some_and(|parent| parent != i64::from(season)) {
                sql.cards.push_str(" AND 0");
            }
            sql.season = Some(sql.bind(i64::from(season)));
        }
        Ok(sql)
    }
    pub fn coordinate_scope(&self, alias: &str) -> String {
        let mut filter = self
            .season
            .as_ref()
            .map(|season| format!(" AND {alias}.season={season}"))
            .unwrap_or_default();
        if let Some(root) = &self.root {
            filter.push_str(&format!(" AND EXISTS(SELECT 1 FROM library_file_units fu JOIN library_files rf ON rf.file_id=fu.file_id WHERE fu.library_id={alias}.library_id AND fu.media_id={alias}.media_id AND fu.season={alias}.season AND fu.episode={alias}.episode AND rf.root_id={root} AND rf.publication_state='published' AND rf.presence_state='present')"));
        }
        filter
    }
}

pub(super) fn authorized(
    store: &Store,
    user: UserId,
    libraries: &[LibraryId],
) -> Result<Vec<LibraryId>, StoreError> {
    let authorized = store.authorized_library_ids(user)?;
    let mut ids: Vec<_> = libraries
        .iter()
        .copied()
        .filter(|id| authorized.contains(&id.to_string()))
        .collect();
    ids.sort_unstable_by_key(ToString::to_string);
    ids.dedup();
    Ok(ids)
}
pub(super) fn ready(conn: &Connection, libraries: &[LibraryId]) -> Result<(), StoreError> {
    for library in libraries {
        crate::library_query::items::page::ensure_library_query_ready_on(conn, *library)?;
    }
    Ok(())
}
