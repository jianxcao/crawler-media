//! Published + present source selection for playback.
//!
//! `library_files` joined with `library_file_units` is the only authority for
//! "which file plays this item": pending files are management-only facts, and a
//! missing/unknown presence is never a playable source. Every playback consumer
//! (resume resolution, session start, stream routing, source lists) reads
//! through this module so they agree on the same file.

use std::collections::{HashMap, HashSet};

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, MediaId,
    MediaKind, PhysicalSourceId, QualitySource, TypedItemId, UnitKey, UserId,
};
use rusqlite::{OptionalExtension, params_from_iter, types::Value as SqlValue};

use super::super::maps::parse_id;
use super::super::{Store, StoreError};

/// `library_files` source columns shared by every read here, in order. The two
/// trailing columns are the lowest covered unit and the current track revision
/// (the probe's `file_details` row wins over the revision published on the file
/// row, which is the only one an unprobed file has).
const SOURCE_COLUMNS: &str = "f.file_id, f.library_id, f.root_id, f.media_id, f.kind, \
     f.display_path, f.presence_state, f.publication_state, f.file_revision, f.source_version, \
     f.size_bytes, f.stat_known, f.created_ns, f.mtime_ns, f.source_digest, f.source_path, \
     f.transfer_mode, f.resolution, f.codec, f.hdr, f.quality_source, f.confidence, \
     f.filter_score, f.release_quality, f.duration_ms, f.tracks_version, f.poster_path, \
     f.backdrop_path, f.still_path, MIN(u.season), MIN(u.episode), \
     COALESCE(d.tracks_version, f.tracks_version)";

const SOURCE_FROM: &str = "FROM library_files f \
     JOIN library_file_units u ON u.file_id = f.file_id \
     LEFT JOIN file_details d ON d.file_id = f.file_id";

/// Same three tables, with the loop order pinned: units first, then the file row.
///
/// `source_rows` always filters on `u.media_id`, which `coverage_media` seeks in
/// one hop, while the optional user scope adds `f.library_id IN (…)`. Left to the
/// optimizer, that library predicate looks like a cheap way in: it drives
/// `library_files` through `files_presence (library_id, presence_state)`, so a
/// single-item playback read visits **every present file in the Library** before
/// the `media_id` filter discards them (0.029 ms → 56 ms at 61k files).
/// `CROSS JOIN` fixes the join order, so the selective `media_id` seek stays the
/// outer loop and the library predicate is applied to the handful of rows it
/// actually describes.
const SOURCE_FROM_BY_MEDIA: &str = "FROM library_file_units u \
     CROSS JOIN library_files f ON u.file_id = f.file_id \
     LEFT JOIN file_details d ON d.file_id = f.file_id";

/// One row per file: a file that covers several units (a merged episode, a
/// season pack) must not be listed once per covered unit.
const SOURCE_GROUP: &str = "GROUP BY f.file_id";

const SOURCE_ORDER: &str = "ORDER BY f.file_revision DESC, f.file_id ASC";

const SOURCE_FACTS: &str = "f.presence_state = 'present' AND f.publication_state = 'published'";

/// Bound-value batch size for id lists: well under SQLite's variable limit.
const SOURCE_BATCH: usize = 200;

/// One selected source: the file, the lowest unit it covers in the selected
/// scope, and the track revision its resume bindings must match.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackSource {
    pub file: LibraryFile,
    pub unit: UnitKey,
    pub tracks_version: i64,
}

impl Store {
    /// Published + present sources for `item` inside the caller's authorized
    /// libraries, best first (`file_revision DESC, file_id ASC`). Pending,
    /// missing and unknown files are never returned.
    ///
    /// - `Work`: the files of that Media (whole-file coverage for movies/videos;
    ///   every published episode file for a TV work).
    /// - `Season`: every file covering that season.
    /// - `Unit`: exactly the files covering that unit.
    pub fn playable_sources(
        &self,
        user_id: UserId,
        item: &TypedItemId,
    ) -> Result<Vec<LibraryFile>, StoreError> {
        Ok(self
            .playable_entries(user_id, item)?
            .into_iter()
            .map(|source| source.file)
            .collect())
    }

    /// Same selection as [`Store::playable_sources`], keeping each file's lowest
    /// covered unit: work-level entries need a representative unit for their
    /// path and watch state.
    pub fn playable_entries(
        &self,
        user_id: UserId,
        item: &TypedItemId,
    ) -> Result<Vec<PlaybackSource>, StoreError> {
        let allowed = self.authorized_library_ids(user_id)?;
        if allowed.is_empty() {
            return Ok(Vec::new());
        }
        let (media_id, season, episode) = match item {
            TypedItemId::Work(media) => (*media, None, None),
            TypedItemId::Season { media, season } => (*media, Some(*season as i32), None),
            TypedItemId::Unit { media, key } => (*media, Some(key.season), Some(key.episode)),
        };
        self.source_rows(Some(&allowed), media_id, season, episode, None, false)
    }

    /// Same selection as [`Store::playable_entries`] without a user scope:
    /// public artwork routes resolve an item id before any user is known, and
    /// the published/present rule alone decides what exists.
    pub fn published_entries_for_item(
        &self,
        item: &TypedItemId,
    ) -> Result<Vec<PlaybackSource>, StoreError> {
        let (media_id, season, episode) = match item {
            TypedItemId::Work(media) => (*media, None, None),
            TypedItemId::Season { media, season } => (*media, Some(*season as i32), None),
            TypedItemId::Unit { media, key } => (*media, Some(key.season), Some(key.episode)),
        };
        self.source_rows(None, media_id, season, episode, None, false)
    }

    /// Published + present file facts for these ids (any library). Pending,
    /// missing and unknown files are absent from the map.
    pub fn published_files(
        &self,
        file_ids: &[LedgerId],
    ) -> Result<HashMap<LedgerId, LibraryFile>, StoreError> {
        let mut found = HashMap::new();
        for chunk in file_ids.chunks(SOURCE_BATCH) {
            let values: Vec<SqlValue> = chunk
                .iter()
                .map(|file_id| SqlValue::Text(file_id.to_string()))
                .collect();
            let sql = format!(
                "SELECT {SOURCE_COLUMNS} {SOURCE_FROM} \
                 WHERE {SOURCE_FACTS} AND f.file_id IN ({}) {SOURCE_GROUP}",
                placeholders(chunk.len(), 0)
            );
            let mut stmt = self.library.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(values), map_source)?;
            for row in rows {
                let source = row?;
                found.insert(source.file.file_id, source.file);
            }
        }
        Ok(found)
    }

    /// Published + present file for one id inside the caller's authorized
    /// libraries. Unknown, pending, missing and unknown-presence files all
    /// resolve to `None`.
    pub fn visible_file(
        &self,
        user_id: UserId,
        file_id: LedgerId,
    ) -> Result<Option<LibraryFile>, StoreError> {
        let allowed = self.authorized_library_ids(user_id)?;
        if allowed.is_empty() {
            return Ok(None);
        }
        let libraries: Vec<String> = allowed.into_iter().collect();
        let mut values = library_values(&libraries);
        values.push(SqlValue::Text(file_id.to_string()));
        let sql = format!(
            "SELECT {SOURCE_COLUMNS} {SOURCE_FROM} \
             WHERE {SOURCE_FACTS} AND f.library_id IN ({}) AND f.file_id = ?{} {SOURCE_GROUP}",
            placeholders(libraries.len(), 0),
            libraries.len() + 1
        );
        let mut stmt = self.library.prepare(&sql)?;
        let found = stmt
            .query_row(params_from_iter(values), map_source)
            .optional()?;
        Ok(found.map(|source| source.file))
    }

    /// Best published + present source covering `unit` inside `libraries`.
    ///
    /// `preferred` narrows the pick to one exact source revision; a preferred
    /// source that is no longer published/present (or no longer covers the unit)
    /// falls back to the best available source, and the resume binding check
    /// then reports the change.
    pub(crate) fn best_source_for_unit(
        &self,
        libraries: &HashSet<String>,
        media_id: MediaId,
        unit: UnitKey,
        preferred: Option<&PhysicalSourceId>,
    ) -> Result<Option<PlaybackSource>, StoreError> {
        self.source_rows(
            Some(libraries),
            media_id,
            Some(unit.season),
            Some(unit.episode),
            preferred,
            false,
        )
        .map(|sources| sources.into_iter().next())
    }

    /// Track revision a resume binding must record for this file: the probe's
    /// `file_details.tracks_version` when it exists, otherwise the revision
    /// published on the file row. `None` when the file is not a published,
    /// present source any more.
    pub(crate) fn file_tracks_version(&self, file_id: LedgerId) -> Result<Option<i64>, StoreError> {
        self.library
            .query_row(
                "SELECT COALESCE(d.tracks_version, f.tracks_version)
                 FROM library_files f
                 LEFT JOIN file_details d ON d.file_id = f.file_id
                 WHERE f.file_id = ?1
                   AND f.presence_state = 'present' AND f.publication_state = 'published'",
                rusqlite::params![file_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Shared source read: one statement, every id bound as a parameter.
    ///
    /// `libraries` of `None` skips the authorization filter (callers that own
    /// their own visibility gate); `limited` keeps only the best source.
    fn source_rows(
        &self,
        libraries: Option<&HashSet<String>>,
        media_id: MediaId,
        season: Option<i32>,
        episode: Option<i32>,
        preferred: Option<&PhysicalSourceId>,
        limited: bool,
    ) -> Result<Vec<PlaybackSource>, StoreError> {
        let libraries: Vec<String> = match libraries {
            Some(set) if set.is_empty() => return Ok(Vec::new()),
            Some(set) => set.iter().cloned().collect(),
            None => Vec::new(),
        };
        let mut values = vec![SqlValue::Text(media_id.to_string())];
        let mut filters = String::from("u.media_id = ?1");
        if let Some(season) = season {
            values.push(SqlValue::Integer(season as i64));
            filters.push_str(&format!(" AND u.season = ?{}", values.len()));
        }
        if let Some(episode) = episode {
            values.push(SqlValue::Integer(episode as i64));
            filters.push_str(&format!(" AND u.episode = ?{}", values.len()));
        }
        if !libraries.is_empty() {
            filters.push_str(&format!(
                " AND f.library_id IN ({})",
                placeholders(libraries.len(), values.len())
            ));
            values.extend(library_values(&libraries));
        }
        if let Some(preferred) = preferred {
            values.push(SqlValue::Text(preferred.file_id.to_string()));
            filters.push_str(&format!(" AND f.file_id = ?{}", values.len()));
            values.push(SqlValue::Integer(preferred.revision));
            filters.push_str(&format!(" AND f.file_revision = ?{}", values.len()));
        }
        let limit = if limited { " LIMIT 1" } else { "" };
        let sql = source_sql(&filters, limit);
        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values), map_source)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

/// The one statement [`Store::source_rows`] runs. Kept separate so a unit test can
/// pin the join order of the statement that actually executes: the media-first
/// `CROSS JOIN` is a performance contract, not a formatting choice.
fn source_sql(filters: &str, limit: &str) -> String {
    format!(
        "SELECT {SOURCE_COLUMNS} {SOURCE_FROM_BY_MEDIA} \
         WHERE {SOURCE_FACTS} AND {filters} {SOURCE_GROUP} {SOURCE_ORDER}{limit}"
    )
}

/// `?, ?, ...` for `count` ids, continuing after `offset` already-bound values.
fn placeholders(count: usize, offset: usize) -> String {
    (0..count)
        .map(|index| format!("?{}", offset + index + 1))
        .collect::<Vec<_>>()
        .join(",")
}

fn library_values(libraries: &[String]) -> Vec<SqlValue> {
    libraries
        .iter()
        .map(|library| SqlValue::Text(library.clone()))
        .collect()
}

fn map_source(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlaybackSource> {
    let presence: String = row.get(6)?;
    let publication: String = row.get(7)?;
    let kind: String = row.get(4)?;
    let quality_source: String = row.get(20)?;
    let confidence: String = row.get(21)?;
    let file = LibraryFile {
        file_id: parse_id(row.get::<_, String>(0)?, 0)?,
        library_id: parse_id(row.get::<_, String>(1)?, 1)?,
        root_id: row.get(2)?,
        media_id: parse_id(row.get::<_, String>(3)?, 3)?,
        kind: parse_kind(kind, 4)?,
        path: row.get::<_, String>(5)?.into(),
        version: FileVersion {
            revision: row.get(8)?,
            source_version: row.get(9)?,
        },
        presence: FilePresence::from_str(&presence).unwrap_or(FilePresence::Present),
        publication: FilePublication::from_str(&publication).unwrap_or(FilePublication::Published),
        size_bytes: row.get::<_, Option<i64>>(10)?.map(|size| size as u64),
        stat_known: row.get::<_, i64>(11)? != 0,
        created_ns: row.get(12)?,
        modified_ns: row.get(13)?,
        source_digest: row.get(14)?,
        source_path: row.get::<_, Option<String>>(15)?.map(Into::into),
        transfer_mode: row.get(16)?,
        resolution: row.get(17)?,
        codec: row.get(18)?,
        hdr: row.get(19)?,
        quality_source: match quality_source.as_str() {
            "release" => QualitySource::Release,
            _ => QualitySource::Probe,
        },
        confidence: match confidence.as_str() {
            "low" => Confidence::Low,
            _ => Confidence::High,
        },
        filter_score: row.get(22)?,
        release_quality: row.get(23)?,
        duration_ms: row.get(24)?,
        tracks_version: row.get(25)?,
        poster_path: row.get::<_, Option<String>>(26)?.map(Into::into),
        backdrop_path: row.get::<_, Option<String>>(27)?.map(Into::into),
        still_path: row.get::<_, Option<String>>(28)?.map(Into::into),
    };
    let season: i32 = row.get(29)?;
    let episode: i32 = row.get(30)?;
    Ok(PlaybackSource {
        file,
        unit: UnitKey::new(season, episode),
        tracks_version: row.get(31)?,
    })
}

fn parse_kind(raw: String, index: usize) -> rusqlite::Result<MediaKind> {
    match raw.as_str() {
        "movie" => Ok(MediaKind::Movie),
        "tv" => Ok(MediaKind::Tv),
        "video" => Ok(MediaKind::Video),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown media kind {other}"),
            )),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The media-first join order is a performance contract: with the library scope
    /// present, the optimizer otherwise drives `library_files` through
    /// `files_presence (library_id, presence_state)` and visits every present file
    /// in the Library to play one item (56 ms at 61k files). Reverting to a plain
    /// `JOIN` re-opens that hole without changing a single result row, so the
    /// rendered statement is pinned here; `tests/library_query_plans.rs` pins the
    /// plan SQLite derives from it.
    #[test]
    fn source_sql_pins_the_media_first_join_order() {
        let sql = source_sql("u.media_id = ?1 AND f.library_id IN (?2)", "");
        assert!(
            sql.contains(
                "FROM library_file_units u CROSS JOIN library_files f \
                 ON u.file_id = f.file_id"
            ),
            "playback source selection must drive from the media_id seek: {sql}"
        );
        assert!(
            !sql.contains("FROM library_files f"),
            "a library_files-first FROM lets the library scope drive the read: {sql}"
        );
    }

    /// The user scope must never turn "belongs to nothing" into "sees everything".
    ///
    /// Two shapes: (a) the item sits in a Library the member cannot see while the
    /// member does see others — the `f.library_id IN (…)` filter has to drop it, and
    /// that is the exact predicate whose plan the tests above pin; (b) the member can
    /// see no Library at all — the empty scope must short-circuit before any row.
    #[test]
    fn scoped_selection_returns_nothing_for_a_member_without_library_access() {
        use domain::{Media, User, UserId, UserRole};

        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        let root = tmp.path().join("private");
        let library = store
            .create_library(
                MediaKind::Movie,
                "Private",
                &[root.to_str().unwrap()],
                "selected",
                true,
                &[],
            )
            .unwrap();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "Scoped".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        let member = UserId::new();
        store
            .insert_user(&User {
                id: member,
                login: "outsider".into(),
                enabled: true,
                role: UserRole::Member,
            })
            .unwrap();
        crate::test_support::publish_fixture_file(
            &store,
            &crate::test_support::FixtureFile {
                media_id: media.id,
                path: root.join("scoped.mkv").display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();

        let item = TypedItemId::Work(media.id);
        let allowed = store.authorized_library_ids(member).unwrap();
        assert!(
            !allowed.is_empty() && !allowed.contains(&library.id),
            "(a) the member sees other Libraries but not this one: {allowed:?}"
        );
        assert!(
            store.playable_entries(member, &item).unwrap().is_empty(),
            "(a) an invisible Library must select nothing"
        );
        assert_eq!(
            store.published_entries_for_item(&item).unwrap().len(),
            1,
            "(a) the file exists — only the scope hides it"
        );

        // (b) no visible Library at all.
        store
            .library
            .execute(
                "UPDATE libraries SET access_mode = 'selected', member_ids_json = '[]'",
                [],
            )
            .unwrap();
        assert!(
            store.authorized_library_ids(member).unwrap().is_empty(),
            "(b) the member now belongs to no Library"
        );
        assert!(
            store.playable_entries(member, &item).unwrap().is_empty(),
            "(b) an empty scope must select nothing, not everything"
        );
    }
}
