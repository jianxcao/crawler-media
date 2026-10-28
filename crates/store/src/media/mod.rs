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
        let tx = crate::write_tx(&self.library)?;
        let has_external = media.tmdb_id.is_some()
            || media.douban_id.is_some()
            || media.tvdb_id.is_some()
            || media.bangumi_id.is_some()
            || media.anilist_id.is_some();
        let identity_state = if has_external {
            "confirmed"
        } else {
            "provisional"
        };

        for (source, ext_id) in [
            ("tmdb", media.tmdb_id.as_deref()),
            ("douban", media.douban_id.as_deref()),
            ("tvdb", media.tvdb_id.as_deref()),
            ("bangumi", media.bangumi_id.as_deref()),
            ("anilist", media.anilist_id.as_deref()),
        ] {
            if let Some(id) = ext_id {
                let existing: Option<String> = tx
                    .query_row(
                        "SELECT media_id FROM media_aliases WHERE source = ?1 AND kind = ?2 AND external_id = ?3",
                        params![source, media.kind.as_str(), id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(existing_id) = existing
                    && existing_id != media.id.to_string()
                {
                    return Err(StoreError::Protected(format!(
                        "Alias {source}:{id} already registered to media {existing_id}"
                    )));
                }
            }
        }

        tx.execute(
            "INSERT INTO media (media_id, kind, title, year, original_title, identity_state, aliases_display_json, metadata_revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
            params![
                media.id.to_string(),
                media.kind.as_str(),
                media.title,
                media.year.map(i64::from),
                media.original_title,
                identity_state,
                aliases_json(&media.aliases),
            ],
        )?;

        for (source, external_id) in [
            ("tmdb", media.tmdb_id.as_deref()),
            ("douban", media.douban_id.as_deref()),
            ("tvdb", media.tvdb_id.as_deref()),
            ("bangumi", media.bangumi_id.as_deref()),
            ("anilist", media.anilist_id.as_deref()),
        ] {
            if let Some(ext_id) = external_id {
                tx.execute(
                    "INSERT INTO media_aliases (source, kind, external_id, media_id, is_refresh_target)
                     VALUES (?1, ?2, ?3, ?4, 1)",
                    params![source, media.kind.as_str(), ext_id, media.id.to_string()],
                )?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    pub fn undo_ensured_media_if_unreferenced(
        &self,
        media_id: MediaId,
        expected: &Media,
    ) -> Result<bool, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;

        let id_str = media_id.to_string();
        let current = media_row(&tx, media_id)?;
        if current.as_ref() != Some(expected) {
            tracing::debug!(%media_id, "Media changed since it was inserted; skipping delete");
            return Ok(false);
        }

        let has_sub: bool = self.subscribe.query_row(
            "SELECT EXISTS(SELECT 1 FROM subscribes WHERE media_id = ?1)",
            [&id_str],
            |r| r.get(0),
        )?;
        if has_sub {
            return Ok(false);
        }

        let has_files: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM library_files WHERE media_id = ?1)",
            [&id_str],
            |r| r.get(0),
        )?;
        if has_files {
            return Ok(false);
        }

        crate::library_operations::integrity_guard::ensure_media_integrity_writable(&tx, media_id)?;
        tx.execute("DELETE FROM media_aliases WHERE media_id = ?1", [&id_str])?;
        let changed = tx.execute("DELETE FROM media WHERE media_id = ?1", [&id_str])?;
        tx.commit()?;
        Ok(changed > 0)
    }

    pub fn get_media(&self, id: MediaId) -> Result<Option<Media>, StoreError> {
        let sql = format!("{MEDIA_SELECT} WHERE m.media_id = ?1");
        self.library
            .query_row(&sql, params![id.to_string()], map_media)
            .optional()
            .map_err(Into::into)
    }

    pub fn get_media_by_title(&self, title: &str) -> Result<Option<Media>, StoreError> {
        let sql = format!("{MEDIA_SELECT} WHERE m.title = ?1");
        self.library
            .query_row(&sql, params![title], map_media)
            .optional()
            .map_err(Into::into)
    }

    pub fn get_media_by_title_kind_year(
        &self,
        title: &str,
        kind: domain::MediaKind,
        year: Option<u16>,
    ) -> Result<Option<Media>, StoreError> {
        let year_i64 = year.map(i64::from);
        if year_i64.is_some() {
            let sql = format!(
                "{MEDIA_SELECT} WHERE m.title = ?1 AND m.kind = ?2 AND m.year IS ?3 LIMIT 1"
            );
            return self
                .library
                .query_row(&sql, params![title, kind.as_str(), year_i64], map_media)
                .optional()
                .map_err(Into::into);
        }

        let sql_null =
            format!("{MEDIA_SELECT} WHERE m.title = ?1 AND m.kind = ?2 AND m.year IS NULL LIMIT 1");
        if let Some(media) = self
            .library
            .query_row(&sql_null, params![title, kind.as_str()], map_media)
            .optional()?
        {
            return Ok(Some(media));
        }

        let sql_not_null =
            format!("{MEDIA_SELECT} WHERE m.title = ?1 AND m.kind = ?2 AND m.year IS NOT NULL");
        let mut stmt = self.library.prepare(&sql_not_null)?;
        let rows = stmt
            .query_map(params![title, kind.as_str()], map_media)?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.len() == 1 {
            Ok(rows.into_iter().next())
        } else {
            Ok(None)
        }
    }

    pub fn get_media_by_tmdb(&self, tmdb_id: &str) -> Result<Option<Media>, StoreError> {
        self.get_media_by_alias("tmdb", tmdb_id)
    }

    pub fn get_media_by_alias(&self, source: &str, id: &str) -> Result<Option<Media>, StoreError> {
        self.get_media_by_alias_kind(source, id, None)
    }

    pub fn get_media_by_alias_kind(
        &self,
        source: &str,
        id: &str,
        kind: Option<domain::MediaKind>,
    ) -> Result<Option<Media>, StoreError> {
        let source = match source {
            "tmdb" | "douban" | "tvdb" | "bangumi" | "anilist" => source,
            other => {
                return Err(StoreError::Invalid(format!(
                    "unknown alias source: {other}"
                )));
            }
        };
        let sql = match kind {
            Some(_) => format!(
                "{MEDIA_SELECT} WHERE m.media_id = (SELECT media_id FROM media_aliases WHERE source = ?1 AND external_id = ?2 AND kind = ?3 LIMIT 1)"
            ),
            None => format!(
                "{MEDIA_SELECT} WHERE m.media_id = (SELECT media_id FROM media_aliases WHERE source = ?1 AND external_id = ?2 LIMIT 1)"
            ),
        };
        let mut stmt = self.library.prepare(&sql)?;
        let mut rows = match kind {
            Some(k) => stmt.query(params![source, id, k.as_str()])?,
            None => stmt.query(params![source, id])?,
        };
        if let Some(row) = rows.next()? {
            Ok(Some(map_media(row)?))
        } else {
            Ok(None)
        }
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
        let tx = crate::write_tx(&self.library)?;
        crate::library_write::publish::ensure_media_writable(&tx, media.id)?;
        if write_media_is_noop(&tx, media)? {
            tx.commit()?;
            return Ok(());
        }
        write_media(&tx, media)?;
        crate::library_write::reduce::reduce_media_everywhere(&tx, media.id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn update_media_if_matches(
        &self,
        expected: &Media,
        replacement: &Media,
    ) -> Result<bool, StoreError> {
        let tx = crate::write_tx(&self.library)?;
        let current = media_row(&tx, expected.id)?;
        if current.as_ref() != Some(expected) {
            tracing::debug!(media_id = %expected.id, "Media changed since it was written; skipping revert");
            return Ok(false);
        }
        // Same barrier as `update_media`: a revert must not resurrect the identity of
        // a Media that is being merged into a survivor or already retired.
        crate::library_write::publish::ensure_media_writable(&tx, replacement.id)?;
        // A revert to the identity the row already holds changed nothing (`update_media`'s
        // no-op rule), so it must not rebuild the summaries either.
        if write_media_is_noop(&tx, replacement)? {
            tx.commit()?;
            return Ok(true);
        }
        write_media(&tx, replacement)?;
        crate::library_write::reduce::reduce_media_everywhere(&tx, replacement.id)?;
        tx.commit()?;
        Ok(true)
    }

    /// An incoming external id conflicts with `existing` only when `existing` has aliases for
    /// that source/kind and none of them is the incoming id.
    fn alias_set_conflicts(&self, existing: &Media, incoming: &Media) -> Result<bool, StoreError> {
        let sources = ["tmdb", "douban", "tvdb", "bangumi", "anilist"];
        for (source, incoming_id) in sources.into_iter().zip(aliases(incoming)) {
            let Some(incoming_id) = incoming_id else {
                continue;
            };
            let mut stmt = self.library.prepare_cached(
                "SELECT external_id FROM media_aliases WHERE media_id = ?1 AND source = ?2 AND kind = ?3",
            )?;
            let registered = stmt
                .query_map(
                    params![existing.id.to_string(), source, incoming.kind.as_str()],
                    |r| r.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?;
            if !registered.is_empty() && !registered.iter().any(|id| id == incoming_id) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn resolve_scanned_media(&self, incoming: Media) -> Result<Media, StoreError> {
        let mut matched_media_ids: std::collections::HashSet<MediaId> =
            std::collections::HashSet::new();
        for (source, ext_id) in [
            ("tmdb", incoming.tmdb_id.as_deref()),
            ("douban", incoming.douban_id.as_deref()),
            ("tvdb", incoming.tvdb_id.as_deref()),
            ("bangumi", incoming.bangumi_id.as_deref()),
            ("anilist", incoming.anilist_id.as_deref()),
        ] {
            if let Some(id) = ext_id
                && let Some(m) = self.get_media_by_alias_kind(source, id, Some(incoming.kind))?
            {
                matched_media_ids.insert(m.id);
            }
        }

        if matched_media_ids.len() > 1 {
            return Err(identity_conflict(&incoming));
        }

        let has_incoming_alias = aliases(&incoming).iter().any(Option::is_some);
        let existing = if let Some(first_id) = matched_media_ids.into_iter().next() {
            let existing = self
                .get_media(first_id)?
                .ok_or_else(|| StoreError::Missing(format!("media {first_id}")))?;
            // Ownership is decided by the full registered alias set: a retained non-target
            // alias identifies the Media just as the refresh target does.
            if self.alias_set_conflicts(&existing, &incoming)? {
                return Err(identity_conflict(&incoming));
            }
            Some(existing)
        } else if !has_incoming_alias {
            let sql = format!("{MEDIA_SELECT} WHERE m.title = ?1 AND m.kind = ?2");
            let mut stmt = self.library.prepare(&sql)?;
            let candidates: Vec<Media> = stmt
                .query_map(params![incoming.title, incoming.kind.as_str()], map_media)?
                .collect::<Result<Vec<_>, _>>()?;
            let titles: Vec<Media> = candidates
                .into_iter()
                .filter(|existing| {
                    existing.title == incoming.title
                        && incoming.year == existing.year
                        && !aliases_conflict(existing, &incoming)
                })
                .collect();
            if titles.len() > 1 {
                return Err(identity_conflict(&incoming));
            }
            titles.into_iter().next()
        } else {
            if let Some(existing) = self.get_media(incoming.id)? {
                if !aliases_conflict(&existing, &incoming) {
                    Some(existing)
                } else {
                    None
                }
            } else {
                let sql = format!("{MEDIA_SELECT} WHERE m.title = ?1 AND m.kind = ?2");
                let mut stmt = self.library.prepare(&sql)?;
                let candidates: Vec<Media> = stmt
                    .query_map(params![incoming.title, incoming.kind.as_str()], map_media)?
                    .collect::<Result<Vec<_>, _>>()?;
                let titles: Vec<Media> = candidates
                    .into_iter()
                    .filter(|existing| {
                        existing.title == incoming.title
                            && incoming.year == existing.year
                            && !aliases_conflict(existing, &incoming)
                            && !introduces_foreign_alias(existing, &incoming)
                    })
                    .collect();
                if titles.len() == 1 {
                    titles.into_iter().next()
                } else {
                    None
                }
            }
        };

        if let Some(existing) = existing {
            let state: String = self.library.query_row(
                "SELECT identity_state FROM media WHERE media_id = ?1",
                params![existing.id.to_string()],
                |r| r.get(0),
            )?;
            if state == "merging" || state == "retired" {
                return Err(StoreError::Protected(format!(
                    "cannot adopt frozen media {} in state {state}",
                    existing.id
                )));
            }

            let mut canonical = existing.clone();
            canonical.year = canonical.year.or(incoming.year);
            canonical.original_title = canonical.original_title.or(incoming.original_title);
            canonical.tmdb_id = canonical.tmdb_id.or(incoming.tmdb_id);
            canonical.douban_id = canonical.douban_id.or(incoming.douban_id);
            canonical.tvdb_id = canonical.tvdb_id.or(incoming.tvdb_id);
            canonical.bangumi_id = canonical.bangumi_id.or(incoming.bangumi_id);
            canonical.anilist_id = canonical.anilist_id.or(incoming.anilist_id);
            self.update_media(&canonical)?;
            tracing::debug!(media_id = %canonical.id, "Recovered canonical scanned Media identity");
            return Ok(canonical);
        }

        self.insert_media(&incoming)?;
        Ok(incoming)
    }

    pub fn ensure_media(&self, incoming: Media) -> Result<Media, StoreError> {
        self.ensure_media_with_previous(incoming)
            .map(|(media, _)| media)
    }

    pub fn ensure_media_with_previous(
        &self,
        incoming: Media,
    ) -> Result<(Media, Option<Media>), StoreError> {
        let aliases = [
            ("tmdb", incoming.tmdb_id.clone()),
            ("douban", incoming.douban_id.clone()),
            ("tvdb", incoming.tvdb_id.clone()),
            ("bangumi", incoming.bangumi_id.clone()),
            ("anilist", incoming.anilist_id.clone()),
        ];
        for (column, id) in aliases {
            let Some(id) = id else { continue };
            if let Some(existing) =
                self.get_media_by_alias_kind(column, &id, Some(incoming.kind))?
            {
                // A frozen (merging/retired) Media must not be rewritten by a scan:
                // `write_media` would recompute `identity_state` and re-point aliases.
                crate::library_write::publish::ensure_media_writable(&self.library, existing.id)?;
                let merged = ::media::merge(existing.clone(), incoming);
                self.update_media(&merged)?;
                return Ok((merged, Some(existing)));
            }
        }
        self.insert_media(&incoming)?;
        Ok((incoming, None))
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
        let tx = crate::write_tx(&self.library)?;
        crate::library_operations::integrity_guard::ensure_media_integrity_writable(&tx, id)?;
        tx.execute(
            "DELETE FROM media_aliases WHERE media_id = ?1",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM media WHERE media_id = ?1",
            params![id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_media(&self) -> Result<Vec<Media>, StoreError> {
        let sql = format!("{MEDIA_SELECT} ORDER BY m.title, m.media_id");
        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map([], map_media)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

#[cfg(test)]
#[path = "identity_no_op_tests.rs"]
mod identity_no_op_tests;
