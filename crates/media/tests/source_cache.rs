use media::response_cache::{
    CACHE_SOURCES, CacheDbError, CacheRow, CacheSource, delete, details_for_prefixes, get,
    initialize, list, open, put,
};
use std::str::FromStr;

#[test]
fn six_sources_are_isolated_with_same_key() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("cache.db");
    let conn = open(&db_path).expect("open cache.db");

    let test_key = "/movie/42";
    for (i, source) in CACHE_SOURCES.iter().enumerate() {
        let body = format!("body_{i}_{}", source.as_str());
        put(&conn, *source, test_key, &body, 1000 + i as i64, Some(2000)).expect("put");
    }

    // List all
    let all = list(&conn, None).expect("list all");
    assert_eq!(all.len(), 6, "Must contain exactly 6 entries for 6 sources");

    // Check each source gets its own isolated body
    for (i, source) in CACHE_SOURCES.iter().enumerate() {
        let row = get(&conn, *source, test_key)
            .expect("get")
            .expect("must exist");
        assert_eq!(row.source, *source);
        assert_eq!(row.cache_key, test_key);
        assert_eq!(row.body, format!("body_{i}_{}", source.as_str()));
    }

    // Delete one source
    let deleted = delete(&conn, CacheSource::Tmdb, test_key).expect("delete");
    assert!(deleted);
    assert_eq!(get(&conn, CacheSource::Tmdb, test_key).expect("get"), None);

    // Other 5 sources must still be intact
    let remaining = list(&conn, None).expect("list remaining");
    assert_eq!(remaining.len(), 5);
}

#[test]
fn update_preserves_entry_id() {
    let tmp = tempfile::tempdir().unwrap();
    let conn = open(&tmp.path().join("cache.db")).unwrap();

    put(&conn, CacheSource::Tmdb, "/item/1", "body_v1", 100, None).unwrap();
    let row1 = get(&conn, CacheSource::Tmdb, "/item/1").unwrap().unwrap();
    let initial_entry_id = row1.entry_id;

    // Update with new body and new timestamp
    put(
        &conn,
        CacheSource::Tmdb,
        "/item/1",
        "body_v2",
        200,
        Some(500),
    )
    .unwrap();
    let row2 = get(&conn, CacheSource::Tmdb, "/item/1").unwrap().unwrap();

    assert_eq!(
        row2.entry_id, initial_entry_id,
        "UPSERT must preserve entry_id"
    );
    assert_eq!(row2.body, "body_v2");
    assert_eq!(row2.fetched_at, 200);
    assert_eq!(row2.expires_at, Some(500));
}

#[test]
fn prefix_query_matches_only_exact_and_query_parameters() {
    let tmp = tempfile::tempdir().unwrap();
    let conn = open(&tmp.path().join("cache.db")).unwrap();

    put(&conn, CacheSource::Tmdb, "/movie/42", "exact", 100, None).unwrap();
    put(
        &conn,
        CacheSource::Tmdb,
        "/movie/42?language=zh-CN",
        "with_query",
        100,
        None,
    )
    .unwrap();
    put(
        &conn,
        CacheSource::Tmdb,
        "/movie/42/images",
        "subresource",
        100,
        None,
    )
    .unwrap();
    put(
        &conn,
        CacheSource::Tmdb,
        "/movie/420",
        "other_id",
        100,
        None,
    )
    .unwrap();
    put(
        &conn,
        CacheSource::Tmdb,
        "/movie/42_extra",
        "other_underscore",
        100,
        None,
    )
    .unwrap();

    let res = details_for_prefixes(&conn, CacheSource::Tmdb, &["/movie/42".into()]).unwrap();
    let matched_keys: Vec<String> = res.into_iter().map(|r| r.cache_key).collect();

    assert!(matched_keys.contains(&"/movie/42".to_string()));
    assert!(matched_keys.contains(&"/movie/42?language=zh-CN".to_string()));
    assert!(
        !matched_keys.contains(&"/movie/42/images".to_string()),
        "Subresources must not match"
    );
    assert!(
        !matched_keys.contains(&"/movie/420".to_string()),
        "Numbers with prefix must not match"
    );
    assert!(
        !matched_keys.contains(&"/movie/42_extra".to_string()),
        "Underscores must not match"
    );
}

#[test]
fn unknown_source_cannot_become_sql_identifier() {
    assert!(CacheSource::from_str("unknown").is_err());
    assert!(CacheSource::from_str("tmdb_cache; DROP TABLE tmdb_cache").is_err());
}

/// 2026-10-18 之前的 cache.db 声明 `*_cache_recent_idx` / `*_cache_retention_idx`，
/// 当前 DDL 声明 `idx_*_cache_fetched` / `idx_*_cache_retention`。
fn rewrite_indexes_to_pre_cutover_names(path: &std::path::Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    for table in ["tmdb", "douban", "tvdb", "bangumi", "anilist", "fanart"] {
        conn.execute_batch(&format!(
            "DROP INDEX idx_{table}_cache_retention;
             CREATE INDEX {table}_cache_recent_idx ON {table}_cache(fetched_at DESC, entry_id ASC);
             CREATE INDEX {table}_cache_retention_idx ON {table}_cache(fetched_at ASC, entry_id ASC);"
        ))
        .unwrap();
    }
}

/// 切换前的索引名必须仍被拒绝（干净切换拒绝旧布局），且拒绝过程不得丢掉已缓存的行：
/// cache.db 是派生数据，运维按 runbook 重建前原文件保持可读以备回滚。
#[test]
fn pre_cutover_index_names_are_rejected_without_losing_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("cache.db");
    {
        let conn = open(&db_path).expect("open cache.db");
        put(
            &conn,
            CacheSource::Tmdb,
            "/movie/7",
            "body_kept",
            1000,
            None,
        )
        .expect("put");
    }
    rewrite_indexes_to_pre_cutover_names(&db_path);

    let error = open(&db_path).expect_err("pre-cutover index set must be rejected");
    assert!(
        error
            .to_string()
            .contains("missing required index idx_tmdb_cache_retention"),
        "unexpected error: {error}"
    );

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    assert_eq!(
        get(&conn, CacheSource::Tmdb, "/movie/7")
            .expect("get")
            .expect("row must survive")
            .body,
        "body_kept"
    );
}
