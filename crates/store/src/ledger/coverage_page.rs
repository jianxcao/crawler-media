//! Library-local keyset paging counts files before joining their unit coverage.

use super::*;

const MAX_FILES: usize = 200;

pub(super) fn read(
    store: &crate::LibraryConnection<'_>,
    library: LibraryId,
    after: Option<LedgerId>,
    limit: usize,
) -> Result<Vec<FileCoverage>, StoreError> {
    let limit = limit.min(MAX_FILES) as i64;
    if limit == 0 {
        return Ok(Vec::new());
    }
    let sql = page_sql(after.is_some());
    match after {
        Some(after) => {
            store.read_coverage(&sql, params![library.to_string(), after.to_string(), limit])
        }
        None => store.read_coverage(&sql, params![library.to_string(), limit]),
    }
}

fn page_sql(has_cursor: bool) -> String {
    let (cursor, limit) = if has_cursor {
        ("AND file_id > ?2", "?3")
    } else {
        ("", "?2")
    };
    format!(
        "WITH selected_files AS MATERIALIZED (
             SELECT file_id FROM library_files
             WHERE library_id = ?1 {cursor}
             ORDER BY file_id LIMIT {limit}
         )
         SELECT {LIBRARY_FILE_COLUMNS}, u.season, u.episode
         FROM selected_files p
         JOIN library_files f ON f.file_id = p.file_id
         LEFT JOIN library_file_units u ON u.file_id = f.file_id
         ORDER BY f.file_id, u.season, u.episode"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_coverage_page_uses_index() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        for has_cursor in [false, true] {
            let sql = page_sql(has_cursor);
            let details = store
                .with_read(crate::Db::Library, |conn| {
                    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
                    let args = (0..stmt.parameter_count()).map(|_| "200");
                    let details = stmt
                        .query_map(rusqlite::params_from_iter(args), |row| {
                            row.get::<_, String>(3)
                        })
                        .unwrap()
                        .map(Result::unwrap)
                        .collect::<Vec<_>>();
                    Ok(details)
                })
                .unwrap();
            for detail in &details {
                assert!(
                    !detail.starts_with("SCAN library_files")
                        && detail != "SCAN f"
                        && !detail.starts_with("SCAN f "),
                    "unbounded file scan: {details:?}"
                );
            }
        }
    }
}
