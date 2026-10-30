use domain::{Media, MediaId};
use rusqlite::{OptionalExtension, params};

pub(crate) mod alias;
mod retired;
pub use alias::MEDIA_SELECT;

use crate::maps::{aliases_json, map_media};
use crate::{Store, StoreError};

fn aliases(media: &Media) -> [Option<&str>; 5] {
    [
        media.tmdb_id.as_deref(),
        media.douban_id.as_deref(),
        media.tvdb_id.as_deref(),
        media.bangumi_id.as_deref(),
        media.anilist_id.as_deref(),
    ]
}

fn aliases_conflict(a: &Media, b: &Media) -> bool {
    aliases(a)
        .into_iter()
        .zip(aliases(b))
        .any(|(a, b)| a.zip(b).is_some_and(|(a, b)| a != b))
}

fn introduces_foreign_alias(existing: &Media, incoming: &Media) -> bool {
    aliases(incoming)
        .into_iter()
        .zip(aliases(existing))
        .any(|(incoming, existing)| incoming.is_some() && existing.is_none())
}

fn identity_conflict(incoming: &Media) -> StoreError {
    tracing::warn!(media_id = %incoming.id, kind = incoming.kind.as_str(), "Ambiguous scanned Media identity rejected");
    StoreError::Protected("Ambiguous or conflicting scanned Media identity".into())
}

fn media_row(conn: &rusqlite::Connection, id: MediaId) -> Result<Option<Media>, StoreError> {
    let sql = format!("{MEDIA_SELECT} WHERE m.media_id = ?1");
    conn.query_row(&sql, params![id.to_string()], map_media)
        .optional()
        .map_err(Into::into)
}

/// The `(source, external id)` pairs a Media identity owns, in write order.
///
/// One list feeds the writer, the refresh-target bookkeeping **and** [`write_media_is_noop`],
/// so an id can never be written without also being compared.
fn media_alias_ids(media: &Media) -> [(&'static str, Option<&str>); 5] {
    [
        ("tmdb", media.tmdb_id.as_deref()),
        ("douban", media.douban_id.as_deref()),
        ("tvdb", media.tvdb_id.as_deref()),
        ("bangumi", media.bangumi_id.as_deref()),
        ("anilist", media.anilist_id.as_deref()),
    ]
}

/// The identity state [`write_media`] derives from the alias set.
fn media_identity_state(media: &Media) -> &'static str {
    if media_alias_ids(media)
        .iter()
        .any(|(_, external_id)| external_id.is_some())
    {
        "confirmed"
    } else {
        "provisional"
    }
}

/// Bind parameters `?2..?6` of [`WRITE_MEDIA_SQL`], shared with [`PLAN_MEDIA_SQL`].
///
/// The two statements mean the same columns by construction: a new column is added to this
/// list once, and both the write and the no-op check read it.
fn media_write_params(media: &Media) -> [rusqlite::types::Value; 5] {
    use rusqlite::types::Value;
    [
        Value::Text(media.title.clone()),
        Value::from(media.year.map(i64::from)),
        Value::from(media.original_title.clone()),
        Value::Text(aliases_json(&media.aliases)),
        Value::Text(media_identity_state(media).to_string()),
    ]
}

const WRITE_MEDIA_SQL: &str = "UPDATE media SET title = ?2, year = ?3, original_title = ?4, \
     aliases_display_json = ?5, identity_state = ?6 WHERE media_id = ?1";

/// `1` when the row already holds exactly what [`WRITE_MEDIA_SQL`] would store.
///
/// `IS`, not `=`: half of these columns are nullable and `NULL = NULL` is NULL, which would
/// report every Media without a year or an original title as changed.
const PLAN_MEDIA_SQL: &str = "SELECT COALESCE(
        title IS ?2 AND year IS ?3 AND original_title IS ?4
        AND aliases_display_json IS ?5 AND identity_state IS ?6,
    0) FROM media WHERE media_id = ?1";

/// Whether [`write_media`] would change anything at all.
///
/// Covers everything the writer touches: the media row, the `identity_state` copy on every card
/// of that Media, and one alias row per external id. An id that is present on the row but whose
/// alias row went missing is *not* a no-op — the writer would restore it.
fn write_media_is_noop(conn: &rusqlite::Connection, media: &Media) -> Result<bool, StoreError> {
    use rusqlite::types::Value;
    let mut params = vec![Value::Text(media.id.to_string())];
    params.extend(media_write_params(media));
    let columns_current: Option<i64> = conn
        .query_row(PLAN_MEDIA_SQL, rusqlite::params_from_iter(params), |row| {
            row.get(0)
        })
        .optional()?;
    if columns_current != Some(1) {
        return Ok(false);
    }
    let identity_state = media_identity_state(media);
    let stale_card: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_items
                       WHERE media_id = ?1 AND identity_state IS NOT ?2)",
        params![media.id.to_string(), identity_state],
        |row| row.get(0),
    )?;
    if stale_card {
        return Ok(false);
    }
    for (source, external_id) in media_alias_ids(media) {
        let Some(external_id) = external_id else {
            continue;
        };
        let present: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM media_aliases
                           WHERE media_id = ?1 AND source = ?2 AND kind = ?3 AND external_id = ?4)",
            params![
                media.id.to_string(),
                source,
                media.kind.as_str(),
                external_id
            ],
            |row| row.get(0),
        )?;
        if !present {
            return Ok(false);
        }
    }
    Ok(true)
}

fn write_media(conn: &rusqlite::Connection, media: &Media) -> Result<(), StoreError> {
    let identity_state = media_identity_state(media);
    let mut write_params = vec![rusqlite::types::Value::Text(media.id.to_string())];
    write_params.extend(media_write_params(media));
    conn.execute(WRITE_MEDIA_SQL, rusqlite::params_from_iter(write_params))?;
    conn.execute(
        "UPDATE library_items SET identity_state = ?2 WHERE media_id = ?1",
        params![media.id.to_string(), identity_state],
    )?;

    // Preserve non-conflicting existing aliases, update or insert incoming ones
    for (source, external_id) in media_alias_ids(media) {
        if let Some(ext_id) = external_id {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM media_aliases WHERE media_id = ?1 AND source = ?2 AND kind = ?3 AND external_id = ?4)",
                params![media.id.to_string(), source, media.kind.as_str(), ext_id],
                |r| r.get(0),
            )?;
            if !exists {
                let has_target: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM media_aliases WHERE media_id = ?1 AND source = ?2 AND kind = ?3 AND is_refresh_target = 1)",
                    params![media.id.to_string(), source, media.kind.as_str()],
                    |r| r.get(0),
                )?;
                let is_target = if has_target { 0 } else { 1 };
                conn.execute(
                    "INSERT INTO media_aliases (source, kind, external_id, media_id, is_refresh_target)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![source, media.kind.as_str(), ext_id, media.id.to_string(), is_target],
                )?;
            }
        }
    }
    Ok(())
}

impl Store {
    pub fn insert_media(&self, media: &Media) -> Result<(), StoreError> {
        self.with_library_write(|db| db.insert_media(media))
    }

    pub fn undo_ensured_media_if_unreferenced(
        &self,
        media_id: MediaId,
        expected: &Media,
    ) -> Result<bool, StoreError> {
        self.with_reference_write(|library, subscribe| {
            library
                .undo_ensured_media_if_unreferenced_with_references(subscribe, media_id, expected)
        })
    }

    pub fn get_media(&self, id: MediaId) -> Result<Option<Media>, StoreError> {
        self.with_library_read(|db| db.get_media(id))
    }

    pub fn get_media_by_title(&self, title: &str) -> Result<Option<Media>, StoreError> {
        self.with_library_read(|db| db.get_media_by_title(title))
    }

    pub fn get_media_by_title_kind_year(
        &self,
        title: &str,
        kind: domain::MediaKind,
        year: Option<u16>,
    ) -> Result<Option<Media>, StoreError> {
        self.with_library_read(|db| db.get_media_by_title_kind_year(title, kind, year))
    }

    pub fn get_media_by_tmdb(&self, tmdb_id: &str) -> Result<Option<Media>, StoreError> {
        self.with_library_read(|db| db.get_media_by_tmdb(tmdb_id))
    }

    pub fn get_media_by_alias(&self, source: &str, id: &str) -> Result<Option<Media>, StoreError> {
        self.with_library_read(|db| db.get_media_by_alias(source, id))
    }

    pub fn get_media_by_alias_kind(
        &self,
        source: &str,
        id: &str,
        kind: Option<domain::MediaKind>,
    ) -> Result<Option<Media>, StoreError> {
        self.with_library_read(|db| db.get_media_by_alias_kind(source, id, kind))
    }

    /// Base identity edit; cards and terms of every owning Library are reduced in
    /// the same transaction (reduction keeps lock/NFO/adopted precedence).
    /// Rewrite one Media identity and republish every summary derived from it.
    ///
    /// Identical input is a **no-op**: the row is not rewritten and the derived summaries are
    /// not rebuilt. That is not just an optimisation — a scan adopts the same Media once per
    /// file, and `reduce_media_everywhere` re-reads every published file of that Media, so a
    /// 100-file chunk used to spend seconds rebuilding a card it had just rebuilt. Skipping the
    /// rebuild is safe because everything it publishes (card facts, terms, user summaries, the
    /// query projection) is derived from file facts, and the identity columns those facts could
    /// disagree with are exactly what [`write_media_is_noop`] compares. A caller that wants a
    /// repair uses [`Self::refresh_media_summaries`], not this write.
    pub fn update_media(&self, media: &Media) -> Result<(), StoreError> {
        self.with_library_write(|db| db.update_media(media))
    }

    pub fn update_media_if_matches(
        &self,
        expected: &Media,
        replacement: &Media,
    ) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.update_media_if_matches(expected, replacement))
    }

    pub fn resolve_scanned_media(&self, incoming: Media) -> Result<Media, StoreError> {
        self.with_library_write(|db| db.resolve_scanned_media(incoming))
    }

    pub fn ensure_media(&self, incoming: Media) -> Result<Media, StoreError> {
        self.with_library_write(|db| db.ensure_media(incoming))
    }

    pub fn ensure_media_with_previous(
        &self,
        incoming: Media,
    ) -> Result<(Media, Option<Media>), StoreError> {
        self.with_library_write(|db| db.ensure_media_with_previous(incoming))
    }

    pub fn undo_ensured_media(
        &self,
        media: &Media,
        previous: Option<&Media>,
    ) -> Result<bool, StoreError> {
        if let Some(previous) = previous {
            self.update_media_if_matches(media, previous)
        } else {
            self.undo_ensured_media_if_unreferenced(media.id, media)
        }
    }

    pub fn delete_media(&self, id: MediaId) -> Result<(), StoreError> {
        self.with_library_write(|db| db.delete_media(id))
    }

    pub fn list_media(&self) -> Result<Vec<Media>, StoreError> {
        self.with_library_read(|db| db.list_media())
    }
}

#[cfg(test)]
#[path = "identity_no_op_tests.rs"]
mod identity_no_op_tests;

#[path = "connection.rs"]
mod connection;
