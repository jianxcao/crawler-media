use std::sync::{Arc, Mutex};

use media::{CatalogGet, Tmdb, TmdbError};

struct LocalizedCatalog(Arc<Mutex<Vec<String>>>);

impl CatalogGet for LocalizedCatalog {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        self.0.lock().unwrap().push(path.to_string());
        let title = if path.ends_with("language=zh-CN") {
            "火线"
        } else {
            "The Wire"
        };
        Ok(serde_json::json!({"results": [{
            "id": 1438, "name": title, "original_name": "The Wire",
            "first_air_date": "2002-06-02"
        }]})
        .to_string())
    }
}

#[test]
fn blank_language_keeps_chinese_default_and_avoids_legacy_english_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("catalog.db");
    let paths = Arc::new(Mutex::new(Vec::new()));
    let client = Tmdb::new_at(LocalizedCatalog(paths.clone()), &db, 100).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "INSERT INTO tmdb_cache (cache_key, body, fetched_at) VALUES (?1, ?2, 100)",
        rusqlite::params![
            "/search/tv?query=wire&first_air_date_year=2002",
            r#"{"results":[{"id":1438,"name":"The Wire","first_air_date":"2002-06-02"}]}"#,
        ],
    ).unwrap();

    for language in ["", "   "] {
        let client = Tmdb::new_at(LocalizedCatalog(paths.clone()), &db, 100)
            .unwrap()
            .with_language(language);
        assert_eq!(
            client.search_tv_year("wire", Some(2002)).unwrap()[0]
                .media
                .title,
            "火线"
        );
    }
    assert_eq!(
        paths.lock().unwrap().as_slice(),
        ["/search/tv?query=wire&first_air_date_year=2002&language=zh-CN"]
    );
    // The configured language still partitions the cache and selects that locale.
    assert_eq!(
        client.with_language("en-US").search_tv("wire").unwrap()[0]
            .media
            .title,
        "The Wire"
    );
}
