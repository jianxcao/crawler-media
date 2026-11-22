//! Paginated, filtered, and STRM-aware transfer ledger queries on `library.db`.

use std::collections::HashMap;
use std::path::Path;

use domain::{FileCoverage, LedgerId, LibraryId, MediaId, MediaKind};
use rusqlite::{Connection, params_from_iter, types::Value as SqlValue};

use crate::{
    LibraryConnection, Store, StoreError,
    maps::{LIBRARY_FILE_COLUMNS, map_file_unit, parse_id, parse_token},
};

const MAX_PAGE_LIMIT: usize = 200;
const DEFAULT_PAGE_LIMIT: usize = 50;

const IS_STRM_EXPR: &str =
    "(LOWER(f.display_path) LIKE '%.strm' OR LOWER(COALESCE(f.transfer_mode, '')) = 'strm')";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransferViewMode {
    #[default]
    Files,
    Groups,
}

impl TransferViewMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Groups => "groups",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransferModeFilter {
    #[default]
    All,
    Hardlink,
    Copy,
    Move,
    Strm,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransferKindFilter {
    #[default]
    All,
    Movie,
    Tv,
}

#[derive(Clone, Debug, Default)]
pub struct TransferPageQuery {
    pub view: TransferViewMode,
    pub search: Option<String>,
    pub mode: TransferModeFilter,
    pub kind: TransferKindFilter,
    pub media_id: Option<MediaId>,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct TransferSummary {
    pub total_files: u64,
    pub media_count: u64,
    pub movie_files: u64,
    pub tv_files: u64,
    pub strm_count: u64,
    pub hardlink_count: u64,
    pub copy_count: u64,
    pub move_count: u64,
    pub local_size_bytes: u64,
    pub probed_duration_ms: u64,
    pub strm_probed_count: u64,
}

#[derive(Clone, Debug)]
pub struct TransferFileRow {
    pub coverage: FileCoverage,
    pub media_title: String,
    pub media_kind: MediaKind,
    pub is_strm: bool,
    pub effective_duration_ms: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct TransferGroupRow {
    pub media_id: MediaId,
    pub media_title: String,
    pub media_kind: MediaKind,
    pub library_id: LibraryId,
    pub file_count: u64,
    pub strm_count: u64,
    pub hardlink_count: u64,
    pub copy_count: u64,
    pub move_count: u64,
    pub local_size_bytes: u64,
    pub total_duration_ms: u64,
    pub primary_resolution: Option<String>,
    pub primary_mode: String,
    pub sample_dir: String,
}

#[derive(Clone, Debug)]
pub struct TransferPage {
    pub global_transfer_mode: String,
    pub summary: TransferSummary,
    pub view: TransferViewMode,
    pub filtered_total: u64,
    pub filtered_file_total: u64,
    pub filtered_group_total: u64,
    pub offset: usize,
    pub limit: usize,
    pub has_more: bool,
    pub files: Vec<TransferFileRow>,
    pub groups: Vec<TransferGroupRow>,
}

impl Store {
    /// Read global summary + one filtered page of files or grouped works from a single
    /// `library.db` snapshot with zero filesystem `stat()` calls.
    pub fn transfer_page(&self, query: &TransferPageQuery) -> Result<TransferPage, StoreError> {
        self.with_library_read(|db| read_transfer_page(db, query))
    }
}

fn read_transfer_page(
    db: &LibraryConnection<'_>,
    query: &TransferPageQuery,
) -> Result<TransferPage, StoreError> {
    let global_transfer_mode = db.transfer_settings()?.transfer_mode;
    let limit = if query.limit == 0 {
        DEFAULT_PAGE_LIMIT
    } else {
        query.limit.min(MAX_PAGE_LIMIT)
    };
    let offset = query.offset;
    let conn = db.library;

    let summary = read_summary(conn, &global_transfer_mode)?;
    let (matching_files, matching_groups) =
        read_filtered_counts(conn, query, &global_transfer_mode)?;
    let filtered_total = match query.view {
        TransferViewMode::Files => matching_files,
        TransferViewMode::Groups => matching_groups,
    };

    if filtered_total == 0 || (offset as u64) >= filtered_total {
        return Ok(TransferPage {
            global_transfer_mode,
            summary,
            view: query.view,
            filtered_total,
            filtered_file_total: matching_files,
            filtered_group_total: matching_groups,
            offset,
            limit,
            has_more: false,
            files: Vec::new(),
            groups: Vec::new(),
        });
    }

    let (files, groups, returned_len) = match query.view {
        TransferViewMode::Files => {
            let items = read_files_page(conn, query, &global_transfer_mode, offset, limit)?;
            let len = items.len();
            (items, Vec::new(), len)
        }
        TransferViewMode::Groups => {
            let rows = read_groups_page(conn, query, &global_transfer_mode, offset, limit)?;
            let len = rows.len();
            (Vec::new(), rows, len)
        }
    };

    let has_more = (offset as u64).saturating_add(returned_len as u64) < filtered_total;
    Ok(TransferPage {
        global_transfer_mode,
        summary,
        view: query.view,
        filtered_total,
        filtered_file_total: matching_files,
        filtered_group_total: matching_groups,
        offset,
        limit,
        has_more,
        files,
        groups,
    })
}

fn i64_to_u64(value: i64) -> u64 {
    u64::try_from(value.max(0)).unwrap_or(0)
}

fn read_summary(conn: &Connection, global_mode: &str) -> Result<TransferSummary, StoreError> {
    let sql = format!(
        "SELECT
             COUNT(*),
             COUNT(DISTINCT f.media_id),
             COALESCE(SUM(CASE WHEN m.kind = 'movie' THEN 1 ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN m.kind = 'tv' THEN 1 ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN {IS_STRM_EXPR} THEN 1 ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN NOT {IS_STRM_EXPR}
                                AND COALESCE(NULLIF(f.transfer_mode, ''), ?1) = 'hardlink' THEN 1 ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN NOT {IS_STRM_EXPR}
                                AND COALESCE(NULLIF(f.transfer_mode, ''), ?1) = 'copy' THEN 1 ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN NOT {IS_STRM_EXPR}
                                AND COALESCE(NULLIF(f.transfer_mode, ''), ?1) = 'move' THEN 1 ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN NOT LOWER(f.display_path) LIKE '%.strm'
                                AND NOT {IS_STRM_EXPR}
                                AND COALESCE(f.size_bytes, 0) > 0 THEN f.size_bytes ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN COALESCE(f.duration_ms, d.format_duration_ms, 0) > 0
                                THEN COALESCE(f.duration_ms, d.format_duration_ms, 0) ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN {IS_STRM_EXPR}
                                AND COALESCE(f.duration_ms, d.format_duration_ms, 0) > 0 THEN 1 ELSE 0 END), 0)
         FROM library_files f
         JOIN media m ON m.media_id = f.media_id
         LEFT JOIN file_details d ON d.file_id = f.file_id
         WHERE NOT EXISTS (
             SELECT 1 FROM library_removals r WHERE r.library_id = f.library_id
         )"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    Ok(stmt.query_row([global_mode], |row| {
        Ok(TransferSummary {
            total_files: i64_to_u64(row.get(0)?),
            media_count: i64_to_u64(row.get(1)?),
            movie_files: i64_to_u64(row.get(2)?),
            tv_files: i64_to_u64(row.get(3)?),
            strm_count: i64_to_u64(row.get(4)?),
            hardlink_count: i64_to_u64(row.get(5)?),
            copy_count: i64_to_u64(row.get(6)?),
            move_count: i64_to_u64(row.get(7)?),
            local_size_bytes: i64_to_u64(row.get(8)?),
            probed_duration_ms: i64_to_u64(row.get(9)?),
            strm_probed_count: i64_to_u64(row.get(10)?),
        })
    })?)
}

fn append_filter_clauses(
    query: &TransferPageQuery,
    global_mode: &str,
    params: &mut Vec<SqlValue>,
) -> String {
    let mut clauses: Vec<String> = vec![
        "NOT EXISTS (SELECT 1 FROM library_removals r WHERE r.library_id = f.library_id)".into(),
    ];
    let mut push = |val: SqlValue| -> usize {
        params.push(val);
        params.len()
    };

    if let Some(media_id) = query.media_id {
        let idx = push(SqlValue::Text(media_id.to_string()));
        clauses.push(format!("f.media_id = ?{idx}"));
    }

    match query.kind {
        TransferKindFilter::All => {}
        TransferKindFilter::Movie => {
            let idx = push(SqlValue::Text("movie".into()));
            clauses.push(format!("m.kind = ?{idx}"));
        }
        TransferKindFilter::Tv => {
            let idx = push(SqlValue::Text("tv".into()));
            clauses.push(format!("m.kind = ?{idx}"));
        }
    }

    match query.mode {
        TransferModeFilter::All => {}
        TransferModeFilter::Strm => {
            clauses.push(IS_STRM_EXPR.to_string());
        }
        TransferModeFilter::Hardlink | TransferModeFilter::Copy | TransferModeFilter::Move => {
            let mode_str = match query.mode {
                TransferModeFilter::Hardlink => "hardlink",
                TransferModeFilter::Copy => "copy",
                TransferModeFilter::Move => "move",
                TransferModeFilter::All | TransferModeFilter::Strm => "",
            };
            let global_idx = push(SqlValue::Text(global_mode.to_string()));
            let mode_idx = push(SqlValue::Text(mode_str.into()));
            clauses.push(format!(
                "(NOT {IS_STRM_EXPR} AND COALESCE(NULLIF(f.transfer_mode, ''), ?{global_idx}) = ?{mode_idx})"
            ));
        }
    }

    if let Some(needle) = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
    {
        let idx = push(SqlValue::Text(needle));
        clauses.push(format!(
            "(INSTR(casefold(m.title), ?{idx}) > 0 \
              OR INSTR(casefold(f.display_path), ?{idx}) > 0 \
              OR INSTR(casefold(COALESCE(f.source_path, '')), ?{idx}) > 0 \
              OR INSTR(casefold(COALESCE(f.resolution, '')), ?{idx}) > 0 \
              OR INSTR(casefold(COALESCE(f.codec, '')), ?{idx}) > 0)"
        ));
    }

    if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    }
}

fn read_filtered_counts(
    conn: &Connection,
    query: &TransferPageQuery,
    global_mode: &str,
) -> Result<(u64, u64), StoreError> {
    let mut params = Vec::new();
    let where_sql = append_filter_clauses(query, global_mode, &mut params);
    let sql = format!(
        "SELECT COUNT(*), COUNT(DISTINCT f.media_id)
         FROM library_files f
         JOIN media m ON m.media_id = f.media_id
         {where_sql}"
    );
    Ok(conn.query_row(&sql, params_from_iter(params), |row| {
        Ok((i64_to_u64(row.get(0)?), i64_to_u64(row.get(1)?)))
    })?)
}

fn read_files_page(
    conn: &Connection,
    query: &TransferPageQuery,
    global_mode: &str,
    offset: usize,
    limit: usize,
) -> Result<Vec<TransferFileRow>, StoreError> {
    let mut params = Vec::new();
    let where_sql = append_filter_clauses(query, global_mode, &mut params);
    params.push(SqlValue::Integer(limit as i64));
    let limit_idx = params.len();
    params.push(SqlValue::Integer(offset as i64));
    let offset_idx = params.len();

    let order_sql = if query.media_id.is_some() {
        "COALESCE((SELECT u0.season FROM library_file_units u0 WHERE u0.file_id = f.file_id ORDER BY u0.season ASC, u0.episode ASC LIMIT 1), 999999) ASC, \
         COALESCE((SELECT u0.episode FROM library_file_units u0 WHERE u0.file_id = f.file_id ORDER BY u0.season ASC, u0.episode ASC LIMIT 1), 999999) ASC, \
         f.display_path ASC, f.file_id ASC"
    } else {
        "COALESCE(f.created_ns, f.mtime_ns, 0) DESC, m.title ASC, f.display_path ASC, f.file_id ASC"
    };

    let sql = format!(
        "WITH page AS MATERIALIZED (
             SELECT
                 f.file_id,
                 m.title AS media_title,
                 m.kind AS media_kind,
                 ROW_NUMBER() OVER (ORDER BY {order_sql}) AS page_ord
             FROM library_files f
             JOIN media m ON m.media_id = f.media_id
             {where_sql}
             ORDER BY {order_sql}
             LIMIT ?{limit_idx} OFFSET ?{offset_idx}
         )
         SELECT
             {LIBRARY_FILE_COLUMNS},
             u.season,
             u.episode,
             p.media_title,
             p.media_kind,
             COALESCE(f.duration_ms, d.format_duration_ms)
         FROM page p
         CROSS JOIN library_files f ON f.file_id = p.file_id
         LEFT JOIN file_details d ON d.file_id = p.file_id
         LEFT JOIN library_file_units u ON u.file_id = p.file_id
         ORDER BY p.page_ord ASC, u.season ASC, u.episode ASC"
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut rows_iter = stmt.query(params_from_iter(params))?;
    let mut slots: HashMap<LedgerId, usize> = HashMap::new();
    let mut items: Vec<TransferFileRow> = Vec::with_capacity(limit);

    while let Some(row) = rows_iter.next()? {
        let (file, path_key, unit) = map_file_unit(row)?;
        let slot = match slots.get(&file.file_id) {
            Some(existing) => *existing,
            None => {
                let media_title: String = row.get(32)?;
                let kind_raw: String = row.get(33)?;
                let media_kind: MediaKind = kind_raw.parse().map_err(StoreError::Invalid)?;
                let raw_duration: Option<i64> = row.get(34)?;
                let is_strm = is_strm_file(&file.path, file.transfer_mode.as_deref());
                slots.insert(file.file_id, items.len());
                items.push(TransferFileRow {
                    coverage: FileCoverage {
                        file,
                        path_key,
                        units: Vec::new(),
                    },
                    media_title,
                    media_kind,
                    is_strm,
                    effective_duration_ms: raw_duration.filter(|&ms| ms > 0),
                });
                items.len() - 1
            }
        };
        if let Some(unit) = unit {
            items[slot].coverage.units.push(unit);
        }
    }

    for item in &mut items {
        item.coverage
            .units
            .sort_unstable_by_key(|unit| (unit.season, unit.episode));
        item.coverage.units.dedup();
    }
    Ok(items)
}

fn read_groups_page(
    conn: &Connection,
    query: &TransferPageQuery,
    global_mode: &str,
    offset: usize,
    limit: usize,
) -> Result<Vec<TransferGroupRow>, StoreError> {
    let mut params = vec![SqlValue::Text(global_mode.to_string())];
    let where_sql = append_filter_clauses(query, global_mode, &mut params);
    params.push(SqlValue::Integer(limit as i64));
    let limit_idx = params.len();
    params.push(SqlValue::Integer(offset as i64));
    let offset_idx = params.len();

    let sql = format!(
        "SELECT
             f.media_id,
             m.title,
             m.kind,
             MIN(f.library_id),
             COUNT(*),
             COALESCE(SUM(CASE WHEN {IS_STRM_EXPR} THEN 1 ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN NOT {IS_STRM_EXPR}
                                AND COALESCE(NULLIF(f.transfer_mode, ''), ?1) = 'hardlink' THEN 1 ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN NOT {IS_STRM_EXPR}
                                AND COALESCE(NULLIF(f.transfer_mode, ''), ?1) = 'copy' THEN 1 ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN NOT {IS_STRM_EXPR}
                                AND COALESCE(NULLIF(f.transfer_mode, ''), ?1) = 'move' THEN 1 ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN NOT LOWER(f.display_path) LIKE '%.strm'
                                AND NOT {IS_STRM_EXPR}
                                AND COALESCE(f.size_bytes, 0) > 0 THEN f.size_bytes ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN COALESCE(f.duration_ms, d.format_duration_ms, 0) > 0
                                THEN COALESCE(f.duration_ms, d.format_duration_ms, 0) ELSE 0 END), 0),
             MAX(f.resolution),
             MIN(f.display_path)
         FROM library_files f
         JOIN media m ON m.media_id = f.media_id
         LEFT JOIN file_details d ON d.file_id = f.file_id
         {where_sql}
         GROUP BY f.media_id, m.title, m.kind
         ORDER BY MAX(COALESCE(f.created_ns, f.mtime_ns, 0)) DESC, m.title ASC, f.media_id ASC
         LIMIT ?{limit_idx} OFFSET ?{offset_idx}"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(params), |row| {
            let media_id: MediaId = parse_id(row.get(0)?, 0)?;
            let media_title: String = row.get(1)?;
            let media_kind: MediaKind = parse_token(row.get(2)?, 2)?;
            let library_id: LibraryId = parse_id(row.get(3)?, 3)?;
            let file_count = i64_to_u64(row.get(4)?);
            let strm_count = i64_to_u64(row.get(5)?);
            let hardlink_count = i64_to_u64(row.get(6)?);
            let copy_count = i64_to_u64(row.get(7)?);
            let move_count = i64_to_u64(row.get(8)?);
            let local_size_bytes = i64_to_u64(row.get(9)?);
            let total_duration_ms = i64_to_u64(row.get(10)?);
            let primary_resolution: Option<String> = row.get(11)?;
            let min_path: String = row.get(12)?;

            let primary_mode = resolve_primary_mode(
                file_count,
                strm_count,
                hardlink_count,
                move_count,
                copy_count,
                global_mode,
            );
            let sample_dir = Path::new(&min_path)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();

            Ok(TransferGroupRow {
                media_id,
                media_title,
                media_kind,
                library_id,
                file_count,
                strm_count,
                hardlink_count,
                copy_count,
                move_count,
                local_size_bytes,
                total_duration_ms,
                primary_resolution,
                primary_mode,
                sample_dir,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn is_strm_file(path: &Path, saved_mode: Option<&str>) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("strm"))
        || saved_mode.is_some_and(|mode| mode.eq_ignore_ascii_case("strm"))
}

fn resolve_primary_mode(
    file_count: u64,
    strm_count: u64,
    hardlink_count: u64,
    move_count: u64,
    copy_count: u64,
    global_mode: &str,
) -> String {
    if file_count == 0 {
        return global_mode.to_string();
    }
    if strm_count == file_count {
        return "strm".to_string();
    }
    if hardlink_count == file_count {
        return "hardlink".to_string();
    }
    if move_count == file_count {
        return "move".to_string();
    }
    if copy_count == file_count {
        return "copy".to_string();
    }
    [
        ("strm", strm_count),
        ("hardlink", hardlink_count),
        ("move", move_count),
        ("copy", copy_count),
    ]
    .into_iter()
    .max_by_key(|(_, count)| *count)
    .map(|(mode, _)| mode.to_string())
    .unwrap_or_else(|| global_mode.to_string())
}
