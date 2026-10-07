use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use domain::MediaKind;
use media::{CatalogGet, Tmdb, TmdbError};

struct FixtureTmdb {
    bodies: HashMap<String, String>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl CatalogGet for FixtureTmdb {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        self.requests.lock().unwrap().push(path.to_string());
        let key = path
            .split_once("?language=")
            .map(|(key, _)| key)
            .or_else(|| path.split_once("&language=").map(|(key, _)| key))
            .unwrap_or(path);
        self.bodies
            .get(key)
            .cloned()
            .ok_or_else(|| TmdbError::Http(format!("missing fixture for {key}")))
    }
}

fn tmdb(dir: &std::path::Path, requests: Arc<Mutex<Vec<String>>>) -> Tmdb<FixtureTmdb> {
    let bodies = HashMap::from([
        (
            "/movie/603?append_to_response=credits,release_dates,translations".to_string(),
            include_str!("fixtures/rich_movie_details.json").to_string(),
        ),
        (
            "/tv/1396?append_to_response=aggregate_credits,content_ratings,translations"
                .to_string(),
            include_str!("fixtures/rich_tv_details.json").to_string(),
        ),
        (
            "/tv/1396?append_to_response=translations,alternative_titles".to_string(),
            include_str!("fixtures/rich_tv_details.json").to_string(),
        ),
        (
            "/tv/1396/season/1".to_string(),
            include_str!("fixtures/rich_season_details.json").to_string(),
        ),
    ]);
    Tmdb::new(FixtureTmdb { bodies, requests }, &dir.join("catalog.db")).unwrap()
}

#[test]
fn movie_details_include_full_catalog_metadata_and_prefer_chinese_certification() {
    let dir = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let metadata = tmdb(dir.path(), requests.clone())
        .details_with_meta(MediaKind::Movie, "603")
        .unwrap();

    assert_eq!(metadata.release_date.as_deref(), Some("1999-03-30"));
    assert_eq!(metadata.last_air_date, None);
    assert_eq!(
        metadata.studios,
        ["Warner Bros. Pictures", "Village Roadshow Pictures"]
    );
    assert_eq!(metadata.content_rating.as_deref(), Some("PG-13"));
    assert_eq!(metadata.original_language.as_deref(), Some("en"));
    assert_eq!(metadata.status.as_deref(), Some("Released"));
    assert_eq!(metadata.vote_count, Some(24_000));
    assert_eq!(
        metadata.tagline.as_deref(),
        Some("Welcome to the Real World.")
    );
    assert_eq!(metadata.number_of_seasons, None);
    assert_eq!(metadata.number_of_episodes, None);
    assert_eq!(metadata.directors, ["Lana Wachowski", "Lilly Wachowski"]);
    assert!(metadata.creators.is_empty());
    assert_eq!(metadata.runtime_minutes.as_deref(), Some("136"));
    assert_eq!(metadata.cast[0].order, 0);
    assert_eq!(metadata.cast[1].order, 1);
    assert_eq!(
        requests.lock().unwrap().as_slice(),
        ["/movie/603?append_to_response=credits,release_dates,translations&language=zh-CN"]
    );
}

#[test]
fn tv_details_include_networks_series_facts_and_us_certification_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let metadata = tmdb(dir.path(), requests.clone())
        .details_with_meta(MediaKind::Tv, "1396")
        .unwrap();

    assert_eq!(metadata.release_date.as_deref(), Some("2008-01-20"));
    assert_eq!(metadata.last_air_date.as_deref(), Some("2013-09-29"));
    assert_eq!(metadata.studios, ["AMC", "Sony Pictures Television"]);
    assert_eq!(metadata.content_rating.as_deref(), Some("TV-14"));
    assert_eq!(metadata.original_language.as_deref(), Some("en"));
    assert_eq!(metadata.status.as_deref(), Some("Ended"));
    assert_eq!(metadata.vote_count, Some(15_000));
    assert_eq!(metadata.overview.as_deref(), Some("化学老师走上犯罪之路。"));
    assert_eq!(metadata.tagline.as_deref(), Some("记住我的名字。"));
    assert_eq!(metadata.number_of_seasons, Some(5));
    assert_eq!(metadata.number_of_episodes, Some(62));
    assert_eq!(metadata.creators, ["Vince Gilligan"]);
    assert!(metadata.directors.is_empty());
    assert_eq!(metadata.episode_run_time, [47, 48]);
    assert_eq!(metadata.runtime_minutes.as_deref(), Some("47"));
    assert_eq!(metadata.cast[0].order, 0);
    assert_eq!(metadata.cast[1].order, 1);
    assert_eq!(
        requests.lock().unwrap().as_slice(),
        [
            "/tv/1396?append_to_response=aggregate_credits,content_ratings,translations&language=zh-CN"
        ]
    );
}

#[test]
fn details_with_meta_for_obeys_language_and_country_priorities() {
    let dir = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let metadata = tmdb(dir.path(), requests.clone())
        .details_with_meta_for(
            MediaKind::Tv,
            "1396",
            &["fr-FR", "zh-CN"],
            &["GB", "US", "CN"],
        )
        .unwrap();

    assert_eq!(
        metadata.overview.as_deref(),
        Some("Un professeur de chimie devient criminel.")
    );
    assert_eq!(metadata.tagline.as_deref(), Some("记住我的名字。"));
    assert_eq!(metadata.content_rating.as_deref(), Some("15"));
    assert_eq!(
        requests.lock().unwrap().as_slice(),
        [
            "/tv/1396?append_to_response=aggregate_credits,content_ratings,translations&language=fr-FR"
        ]
    );
}

#[test]
fn tv_seasons_and_episode_metadata_include_artwork_dates_ratings_and_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let client = tmdb(dir.path(), requests);

    let seasons = client.tv_seasons("1396").unwrap();
    assert_eq!(seasons.len(), 2);
    assert_eq!(seasons[0].overview.as_deref(), Some("The first season."));
    assert_eq!(seasons[0].poster_path.as_deref(), Some("/season-1.jpg"));

    let episodes = client.season_details("1396", 1).unwrap();
    assert_eq!(episodes.len(), 2);
    assert_eq!(episodes[0].air_date.as_deref(), Some("2008-01-20"));
    assert_eq!(episodes[0].runtime_minutes, Some(58));
    assert_eq!(episodes[0].rating.as_deref(), Some("8.9"));
    assert_eq!(episodes[0].vote_count, Some(9_800));
    assert_eq!(episodes[1].overview, None);
}

#[test]
fn season_details_with_explicit_language_sends_language_once() {
    let dir = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let client = tmdb(dir.path(), requests.clone());

    client
        .season_details_lang("1396", 1, Some("zh-CN"))
        .unwrap();

    assert_eq!(
        requests.lock().unwrap().as_slice(),
        ["/tv/1396/season/1?language=zh-CN"]
    );
}
