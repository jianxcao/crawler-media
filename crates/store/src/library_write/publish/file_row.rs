//! One file's row: decide whether the publication would write it, then write it.
//!
//! A rescan republishes files whose facts did not move, so "would this write change anything"
//! is the first question the publication loop asks about a file. The answer comes from
//! [`PLAN_FILE_ROW_SQL`], whose comparisons mirror every assignment in
//! [`UPDATE_FILE_ROW_SQL`] — both are built from [`file_row_params`], so a column cannot be
//! written and forgotten by the check.

use domain::{FileVersion, LedgerId, LibraryFile, UnitKey};
use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;

/// What applying a planned file-row write would do.
pub(super) enum RowPlan {
    Insert,
    Update {
        revision: i64,
        /// No *summary* fact moved, matching `PublishReport::unchanged`'s historical meaning.
        report_unchanged: bool,
    },
    /// Every column `update_file_row` would write already holds this value.
    Current,
}

/// Decide what `update_file_row` would do, without doing it.
///
/// The `row_is_current` expression is the UPDATE's own column expressions compared against
/// the stored row, so "current" means the write would store exactly the values already there —
/// it cannot drift from [`update_file_row`] as long as the two lists are edited together.
/// `report_unchanged` keeps the historical meaning of `PublishReport::unchanged`: no *summary*
/// fact moved (a revision bump or a presence/publication/size/resolution change), even when an
/// artwork path still needed writing.
pub(super) fn plan_file_row(
    tx: &Connection,
    file: &LibraryFile,
    expected: Option<&FileVersion>,
) -> Result<RowPlan, StoreError> {
    let file_id = file.file_id.to_string();
    let mut params = file_row_params(tx, file);
    params.push(rusqlite::types::Value::Text(file_id.clone()));
    let current = tx
        .query_row(PLAN_FILE_ROW_SQL, rusqlite::params_from_iter(params), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, Option<i32>>(10)?,
                r.get::<_, i64>(11)?,
            ))
        })
        .optional()?;

    let Some((
        cur_rev,
        cur_source,
        cur_presence,
        cur_publication,
        cur_size,
        cur_resolution,
        cur_codec,
        cur_hdr,
        cur_quality_source,
        cur_confidence,
        cur_score,
        row_is_current,
    )) = current
    else {
        return Ok(RowPlan::Insert);
    };

    if let Some(exp) = expected
        && (exp.revision != cur_rev || exp.source_version != cur_source)
    {
        return Err(StoreError::Protected(format!(
            "StaleVersion for file {file_id}: expected r{}v{}, got r{cur_rev}v{cur_source}",
            exp.revision, exp.source_version
        )));
    }
    if row_is_current != 0 {
        return Ok(RowPlan::Current);
    }
    // Only a content/source change advances the revision, so resume bindings stay valid
    // across metadata, presence and artwork refreshes.
    let bump_revision = cur_source != file.version.source_version;
    let revision = if bump_revision { cur_rev + 1 } else { cur_rev };
    let summary_unchanged = !bump_revision
        && cur_presence == file.presence.as_str()
        && cur_publication == file.publication.as_str()
        && cur_size == file.size_bytes.map(|s| s as i64)
        && cur_resolution == file.resolution
        && cur_codec == file.codec
        && cur_hdr == file.hdr
        && cur_quality_source == file.quality_source.as_str()
        && cur_confidence == file.confidence.as_str()
        && cur_score == file.filter_score;
    Ok(RowPlan::Update {
        revision,
        report_unchanged: summary_unchanged,
    })
}

/// The bind parameters shared by [`UPDATE_FILE_ROW_SQL`] and [`PLAN_FILE_ROW_SQL`] (`?1..?28`,
/// the caller appends `?29 = file_id`). One list feeds the write and the no-op check, so a new
/// column cannot be written but forgotten by the comparison.
fn file_row_params(tx: &Connection, file: &LibraryFile) -> Vec<rusqlite::types::Value> {
    use rusqlite::types::Value;
    vec![
        Value::Text(crate::path_key::canonical_recorded_key(tx, &file.path.to_string_lossy())),
        Value::Text(file.path.to_string_lossy().to_string()),
        Value::Text(file.presence.as_str().to_string()),
        Value::Text(file.publication.as_str().to_string()),
        Value::Integer(file.version.revision),
        Value::Text(file.version.source_version.clone()),
        Value::from(file.size_bytes.map(|s| s as i64)),
        Value::Integer(i64::from(file.stat_known)),
        Value::from(file.modified_ns),
        Value::from(file.created_ns),
        Value::from(file.source_digest.clone()),
        Value::from(file.source_path.as_ref().map(|p| p.to_string_lossy().to_string())),
        Value::from(file.transfer_mode.clone()),
        Value::from(file.resolution.clone()),
        Value::from(file.codec.clone()),
        Value::from(file.hdr.clone()),
        Value::Text(file.quality_source.as_str().to_string()),
        Value::Text(file.confidence.as_str().to_string()),
        Value::from(file.filter_score),
        Value::from(file.release_quality.clone()),
        Value::from(file.duration_ms),
        Value::Integer(file.tracks_version),
        Value::from(file.poster_path.as_ref().map(|p| p.to_string_lossy().to_string())),
        Value::from(file.backdrop_path.as_ref().map(|p| p.to_string_lossy().to_string())),
        Value::from(file.still_path.as_ref().map(|p| p.to_string_lossy().to_string())),
        Value::Integer(i64::from(file.poster_path.is_some())),
        Value::Integer(i64::from(file.backdrop_path.is_some())),
        Value::Integer(i64::from(file.still_path.is_some())),
    ]
}

/// The stored row's dispatch-relevant columns, plus whether `UPDATE` would write anything.
///
/// The trailing expression mirrors every assignment in [`update_file_row`]: `COALESCE(?n, col)`
/// columns are "current" when the stored value already equals the merged value, `MAX(col, ?n)`
/// flags when the incoming flag does not raise it, and the rest compare directly.
///
/// Every comparison uses `IS`, not `=` (+ `IS NOT` elsewhere): half of these columns are
/// nullable, and `NULL = NULL` is NULL — which would report "changed" for every row that has
/// no artwork, path or digest and make the whole no-op check useless.
const PLAN_FILE_ROW_SQL: &str = "SELECT file_revision, source_version, presence_state,
        publication_state, size_bytes, resolution, codec, hdr, quality_source, confidence,
        filter_score,
        COALESCE(
            path_key IS norm_path_key(?1) AND display_path IS ?2 AND presence_state IS ?3
            AND publication_state IS ?4 AND file_revision IS ?5 AND source_version IS ?6
            AND size_bytes IS COALESCE(?7, size_bytes)
            AND stat_known IS MAX(stat_known, ?8)
            AND mtime_ns IS COALESCE(?9, mtime_ns) AND created_ns IS COALESCE(?10, created_ns)
            AND source_digest IS COALESCE(?11, source_digest)
            AND source_path IS COALESCE(?12, source_path)
            AND transfer_mode IS COALESCE(?13, transfer_mode)
            AND resolution IS COALESCE(?14, resolution) AND codec IS COALESCE(?15, codec)
            AND hdr IS COALESCE(?16, hdr) AND quality_source IS ?17 AND confidence IS ?18
            AND filter_score IS COALESCE(?19, filter_score)
            AND release_quality IS COALESCE(?20, release_quality)
            AND duration_ms IS COALESCE(?21, duration_ms) AND tracks_version IS ?22
            AND poster_path IS COALESCE(?23, poster_path)
            AND backdrop_path IS COALESCE(?24, backdrop_path)
            AND still_path IS COALESCE(?25, still_path)
            AND has_poster IS MAX(has_poster, ?26) AND has_backdrop IS MAX(has_backdrop, ?27)
            AND has_still IS MAX(has_still, ?28),
        0)
     FROM library_files WHERE file_id = ?29";



pub(crate) fn insert_file_row(tx: &Connection, file: &LibraryFile) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO library_files
            (file_id, library_id, root_id, media_id, kind, path_key, display_path,
             presence_state, publication_state, file_revision, source_version,
             size_bytes, stat_known, mtime_ns, created_ns, source_digest,
             source_path, transfer_mode, resolution, codec, hdr, quality_source, confidence,
             filter_score, release_quality, duration_ms, tracks_version,
             poster_path, backdrop_path, still_path, has_poster, has_backdrop, has_still)
         VALUES
            (?1, ?2, ?3, ?4, ?5, norm_path_key(?6), ?7,
             ?8, ?9, ?10, ?11,
             ?12, ?13, ?14, ?15, ?16,
             ?17, ?18, ?19, ?20, ?21, ?22, ?23,
             ?24, ?25, ?26, ?27,
             ?28, ?29, ?30, ?31, ?32, ?33)",
        params![
            file.file_id.to_string(),
            file.library_id.to_string(),
            file.root_id,
            file.media_id.to_string(),
            file.kind.as_str(),
            crate::path_key::canonical_recorded_key(tx, &file.path.to_string_lossy()),
            file.path.to_string_lossy().to_string(),
            file.presence.as_str(),
            file.publication.as_str(),
            file.version.revision,
            file.version.source_version,
            file.size_bytes.map(|s| s as i64),
            i64::from(file.stat_known),
            file.modified_ns,
            file.created_ns,
            file.source_digest,
            file.source_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            file.transfer_mode,
            file.resolution,
            file.codec,
            file.hdr,
            file.quality_source.as_str(),
            file.confidence.as_str(),
            file.filter_score,
            file.release_quality,
            file.duration_ms,
            file.tracks_version,
            file.poster_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            file.backdrop_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            file.still_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            i64::from(file.poster_path.is_some()),
            i64::from(file.backdrop_path.is_some()),
            i64::from(file.still_path.is_some()),
        ],
    )?;
    Ok(())
}

pub(super) fn update_file_row(tx: &Connection, file: &LibraryFile, revision: i64) -> Result<(), StoreError> {
    // Artwork paths are never cleared by a later publication that does not carry them,
    // and a stat observation never overwrites a known value with NULL. `PLAN_FILE_ROW_SQL`
    // evaluates the same expressions as a predicate, so the two cannot disagree.
    let mut params = file_row_params(tx, file);
    params[4] = rusqlite::types::Value::Integer(revision);
    params.push(rusqlite::types::Value::Text(file.file_id.to_string()));
    tx.execute(UPDATE_FILE_ROW_SQL, rusqlite::params_from_iter(params))?;
    Ok(())
}

const UPDATE_FILE_ROW_SQL: &str = "UPDATE library_files SET
            path_key = norm_path_key(?1), display_path = ?2,
            presence_state = ?3, publication_state = ?4, file_revision = ?5, source_version = ?6,
            size_bytes = COALESCE(?7, size_bytes), stat_known = MAX(stat_known, ?8),
            mtime_ns = COALESCE(?9, mtime_ns), created_ns = COALESCE(?10, created_ns),
            source_digest = COALESCE(?11, source_digest),
            source_path = COALESCE(?12, source_path),
            transfer_mode = COALESCE(?13, transfer_mode),
            resolution = COALESCE(?14, resolution), codec = COALESCE(?15, codec),
            hdr = COALESCE(?16, hdr), quality_source = ?17, confidence = ?18,
            filter_score = COALESCE(?19, filter_score),
            release_quality = COALESCE(?20, release_quality),
            duration_ms = COALESCE(?21, duration_ms), tracks_version = ?22,
            poster_path = COALESCE(?23, poster_path),
            backdrop_path = COALESCE(?24, backdrop_path),
            still_path = COALESCE(?25, still_path),
            has_poster = MAX(has_poster, ?26),
            has_backdrop = MAX(has_backdrop, ?27),
            has_still = MAX(has_still, ?28)
         WHERE file_id = ?29";

pub(crate) fn sync_coverage(
    tx: &Connection,
    file: &LibraryFile,
    units: &[UnitKey],
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM library_file_units WHERE file_id = ?1",
        params![file.file_id.to_string()],
    )?;
    for unit in units {
        tx.execute(
            "INSERT INTO library_file_units (file_id, library_id, media_id, season, episode)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                file.file_id.to_string(),
                file.library_id.to_string(),
                file.media_id.to_string(),
                unit.season,
                unit.episode,
            ],
        )?;
    }
    Ok(())
}

/// Coverage units in the order `read_file_units` returns them, so the comparison is by set.
pub(super) fn sorted_units(units: &[UnitKey]) -> Vec<(i32, i32)> {
    let mut out: Vec<(i32, i32)> = units.iter().map(|u| (u.season, u.episode)).collect();
    out.sort_unstable();
    out
}

pub(crate) fn enqueue_probe(
    tx: &Connection,
    file_id: LedgerId,
    now_ms: i64,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO file_probe_obligations (file_id, created_at_ms, reason)
         VALUES (?1, ?2, 'publish')
         ON CONFLICT(file_id) DO NOTHING",
        params![file_id.to_string(), now_ms],
    )?;
    Ok(())
}

