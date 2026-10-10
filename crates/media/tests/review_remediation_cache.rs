use std::path::Path;
use std::sync::{Arc, Mutex};

use domain::MediaKind;
use media::{Anilist, Bangumi, CatalogGet, Douban, Tmdb, TmdbError, Tvdb};

const NOW: i64 = 100;
const ERROR: &str = r#"{"success":false,"status_code":7,"status_message":"upstream unavailable"}"#;

#[derive(Clone)]
struct Reply(Arc<Mutex<String>>);
impl CatalogGet for Reply {
    fn get(&self, _: &str) -> Result<String, TmdbError> {
        Ok(self.0.lock().unwrap().clone())
    }
}

struct Case {
    name: &'static str,
    body: &'static str,
    run: fn(Reply, &Path, i64) -> Result<String, TmdbError>,
}

fn search_cases() -> Vec<Case> {
    vec![
        Case {
            name: "tmdb",
            body: include_str!("fixtures/search_movie.json"),
            run: |http, db, now| {
                Ok(Tmdb::new_at(http, db, now)?.search_movie("matrix")?[0]
                    .media
                    .title
                    .clone())
            },
        },
        Case {
            name: "douban",
            body: include_str!("fixtures/douban_search_movie.json"),
            run: |http, db, now| {
                Ok(Douban::new_at(http, db, now)?.search_movie("matrix")?[0]
                    .media
                    .title
                    .clone())
            },
        },
        Case {
            name: "tvdb",
            body: include_str!("fixtures/tvdb_search_movie.json"),
            run: |http, db, now| {
                Ok(Tvdb::new_at(http, db, now)?.search_movie("matrix")?[0]
                    .media
                    .title
                    .clone())
            },
        },
        Case {
            name: "bangumi",
            body: include_str!("fixtures/bangumi_search_movie.json"),
            run: |http, db, now| {
                Ok(Bangumi::new_at(http, db, now)?.search_movie("matrix")?[0]
                    .media
                    .title
                    .clone())
            },
        },
        Case {
            name: "anilist",
            body: include_str!("fixtures/anilist_search_movie.json"),
            run: |http, db, now| {
                Ok(Anilist::new_at(http, db, now)?.search_movie("matrix")?[0]
                    .media
                    .title
                    .clone())
            },
        },
    ]
}

fn endpoint_cases() -> Vec<Case> {
    tmdb_details_cases()
        .into_iter()
        .chain(tmdb_artwork_and_configuration_cases())
        .chain(douban_endpoint_cases())
        .collect()
}

fn tmdb_details_cases() -> Vec<Case> {
    vec![
        Case {
            name: "tmdb paged",
            body: r#"{"results":[],"page":2}"#,
            run: |h, db, n| {
                Ok(format!(
                    "{:?}",
                    Tmdb::new_at(h, db, n)?.discover_movie_paged("", 2)?
                ))
            },
        },
        Case {
            name: "tmdb details",
            body: include_str!("fixtures/movie_details.json"),
            run: |h, db, n| Ok(Tmdb::new_at(h, db, n)?.movie_details("603")?.title),
        },
        Case {
            name: "tmdb poster",
            body: r#"{"poster_path":"/poster.jpg"}"#,
            run: |h, db, n| {
                Ok(format!(
                    "{:?}",
                    Tmdb::new_at(h, db, n)?.details_poster(MediaKind::Movie, "603")?
                ))
            },
        },
        Case {
            name: "tmdb metadata",
            body: include_str!("fixtures/rich_movie_details.json"),
            run: |h, db, n| {
                Ok(format!(
                    "{:?}",
                    Tmdb::new_at(h, db, n)?.details_with_meta(MediaKind::Movie, "603")?
                ))
            },
        },
        Case {
            name: "tmdb tv seasons",
            body: include_str!("fixtures/rich_tv_details.json"),
            run: |h, db, n| Ok(format!("{:?}", Tmdb::new_at(h, db, n)?.tv_seasons("1396")?)),
        },
        Case {
            name: "tmdb season episodes",
            body: include_str!("fixtures/rich_season_details.json"),
            run: |h, db, n| {
                Ok(format!(
                    "{:?}",
                    Tmdb::new_at(h, db, n)?.season_episodes("1396", 1)?
                ))
            },
        },
        Case {
            name: "tmdb season metadata",
            body: include_str!("fixtures/rich_season_details.json"),
            run: |h, db, n| {
                Ok(format!(
                    "{:?}",
                    Tmdb::new_at(h, db, n)?.season_details("1396", 1)?
                ))
            },
        },
    ]
}

fn tmdb_artwork_and_configuration_cases() -> Vec<Case> {
    vec![
        Case {
            name: "tmdb episode images",
            body: r#"{"stills":[{"file_path":"/still.jpg"}]}"#,
            run: |h, db, n| {
                Ok(format!(
                    "{:?}",
                    Tmdb::new_at(h, db, n)?.episode_images("1396", 1, 1)?
                ))
            },
        },
        Case {
            name: "tmdb images",
            body: r#"{"posters":[{"file_path":"/poster.jpg"}]}"#,
            run: |h, db, n| {
                Ok(format!(
                    "{:?}",
                    Tmdb::new_at(h, db, n)?.images(MediaKind::Movie, "603")?
                ))
            },
        },
        Case {
            name: "tmdb person",
            body: r#"{"name":"Keanu Reeves","combined_credits":{"cast":[]}}"#,
            run: |h, db, n| Ok(Tmdb::new_at(h, db, n)?.person_details("6384")?.name),
        },
        Case {
            name: "tmdb genres",
            body: r#"{"genres":[{"id":28,"name":"Action"}]}"#,
            run: |h, db, n| {
                Ok(format!(
                    "{:?}",
                    Tmdb::new_at(h, db, n)?.genres(MediaKind::Movie)?
                ))
            },
        },
        Case {
            name: "tmdb countries",
            body: r#"[{"iso_3166_1":"CN"}]"#,
            run: |h, db, n| {
                Ok(format!(
                    "{:?}",
                    Tmdb::new_at(h, db, n)?.configuration_countries()?
                ))
            },
        },
        Case {
            name: "tmdb languages",
            body: r#"[{"iso_639_1":"zh"}]"#,
            run: |h, db, n| {
                Ok(format!(
                    "{:?}",
                    Tmdb::new_at(h, db, n)?.configuration_languages()?
                ))
            },
        },
    ]
}

fn douban_endpoint_cases() -> Vec<Case> {
    vec![
        Case {
            name: "douban popular",
            body: include_str!("fixtures/douban_popular_movie.json"),
            run: |h, db, n| {
                Ok(Douban::new_at(h, db, n)?.popular_movie()?[0]
                    .media
                    .title
                    .clone())
            },
        },
        Case {
            name: "douban collection",
            body: include_str!("fixtures/douban_subject_collection_movie.json"),
            run: |h, db, n| {
                Ok(Douban::new_at(h, db, n)?.trending_movie()?[0]
                    .media
                    .title
                    .clone())
            },
        },
        Case {
            name: "douban detail",
            body: r#"<h1><span property="v:itemreviewed">Movie</span> <span class="year">(1994)</span></h1>"#,
            run: |h, db, n| {
                Ok(Douban::new_at(h, db, n)?
                    .detail("1")?
                    .map(|m| m.title)
                    .unwrap_or_default())
            },
        },
    ]
}

fn cache_rows(db: &Path) -> Vec<(String, String, i64)> {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.prepare("SELECT cache_key, body, fetched_at FROM catalog_cache ORDER BY cache_key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn assert_stale_survives(case: Case, bad: &str) {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("catalog.db");
    let http = Reply(Arc::new(Mutex::new(case.body.into())));
    let expected = (case.run)(http.clone(), &db, NOW).unwrap();
    let before = cache_rows(&db);
    *http.0.lock().unwrap() = bad.into();
    let stale = (case.run)(http.clone(), &db, NOW + media::DEFAULT_TTL_SECS + 1);
    assert_eq!(
        stale.unwrap_or_else(|e| panic!("{}: {e}", case.name)),
        expected,
        "{}",
        case.name
    );
    assert_eq!(
        cache_rows(&db),
        before,
        "{} must retain old body and timestamp",
        case.name
    );
    *http.0.lock().unwrap() = case.body.into();
    assert_eq!(
        (case.run)(http, &db, NOW + media::DEFAULT_TTL_SECS + 2).unwrap(),
        expected
    );
}

#[test]
fn every_metadata_source_preserves_valid_stale_on_malformed_success() {
    for case in search_cases() {
        assert_stale_survives(case, "<html>temporary proxy error</html>");
    }
}

#[test]
fn configuration_error_arrays_do_not_replace_business_rows() {
    for case in tmdb_artwork_and_configuration_cases()
        .into_iter()
        .filter(|case| matches!(case.name, "tmdb countries" | "tmdb languages"))
    {
        assert_stale_survives(case, r#"[{"error":"permission denied"}]"#);
    }
}

#[test]
fn error_status_envelopes_are_not_metadata_bodies_even_with_optional_fields() {
    let case = tmdb_details_cases()
        .into_iter()
        .find(|case| case.name == "tmdb metadata")
        .unwrap();
    assert_stale_survives(case, r#"{"status":"success","message":"request denied"}"#);
}

#[test]
fn tmdb_error_status_code_does_not_become_an_empty_search_body() {
    let case = search_cases()
        .into_iter()
        .find(|case| case.name == "tmdb")
        .unwrap();
    assert_stale_survives(
        case,
        r#"{"status_code":7,"status_message":"invalid key","results":[]}"#,
    );
}

#[test]
fn endpoint_error_envelopes_do_not_replace_valid_stale() {
    for case in endpoint_cases() {
        assert_stale_survives(case, ERROR);
    }
}

#[test]
fn every_metadata_source_retries_preexisting_malformed_fresh_cache() {
    for case in search_cases().into_iter().chain(endpoint_cases()) {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("catalog.db");
        let http = Reply(Arc::new(Mutex::new(case.body.into())));
        let expected = (case.run)(http.clone(), &db, NOW).unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("UPDATE catalog_cache SET body = ?1", [ERROR])
            .unwrap();
        let actual = (case.run)(http, &db, NOW + 1);
        assert_eq!(
            actual.unwrap_or_else(|e| panic!("{}: {e}", case.name)),
            expected,
            "{}",
            case.name
        );
        assert!(
            cache_rows(&db).iter().all(|(_, body, _)| body == case.body),
            "{}",
            case.name
        );
    }
}

#[test]
fn endpoint_decode_failure_without_stale_does_not_poison_recovery() {
    for case in search_cases().into_iter().chain(endpoint_cases()) {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("catalog.db");
        let http = Reply(Arc::new(Mutex::new(ERROR.into())));
        assert!(
            (case.run)(http.clone(), &db, NOW).is_err(),
            "{} accepted error envelope",
            case.name
        );
        assert!(
            cache_rows(&db).is_empty(),
            "{} persisted rejected response",
            case.name
        );
        *http.0.lock().unwrap() = case.body.into();
        assert!(
            (case.run)(http, &db, NOW + 1).is_ok(),
            "{} did not recover",
            case.name
        );
    }
}

#[test]
fn business_field_decode_errors_preserve_stale_for_every_source() {
    for case in search_cases() {
        let mut body: serde_json::Value = serde_json::from_str(case.body).unwrap();
        match case.name {
            "tmdb" => body["results"][0]["id"] = serde_json::json!("not-an-id"),
            "douban" => body[0]["title"] = serde_json::Value::Null,
            "tvdb" => body["data"][0]["name"] = serde_json::Value::Null,
            "bangumi" => {
                body["list"][0]["name_cn"] = serde_json::Value::Null;
                body["list"][0]["name"] = serde_json::Value::Null;
            }
            "anilist" => body["data"]["Page"]["media"][0]["title"] = serde_json::json!({}),
            other => panic!("uncovered Metadata source: {other}"),
        }
        assert_stale_survives(case, &body.to_string());
    }
}

#[test]
fn paged_list_keeps_optional_totals_and_missing_results_tolerance() {
    let temp = tempfile::tempdir().unwrap();
    let http = Reply(Arc::new(Mutex::new(r#"{"page":3,"total_pages":4}"#.into())));
    let tmdb = Tmdb::new_at(http, &temp.path().join("catalog.db"), NOW).unwrap();
    let (hits, page, pages, results) = tmdb.discover_movie_paged("", 3).unwrap();
    assert_eq!((hits.len(), page, pages, results), (0, 3, 4, 0));
}
