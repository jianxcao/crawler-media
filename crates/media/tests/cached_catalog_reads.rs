use std::sync::{Arc, Mutex};

use domain::MediaKind;
use media::{CatalogGet, Tmdb, TmdbError};

const NOW: i64 = 1_000_000;

#[derive(Clone)]
struct Source(Arc<Mutex<Option<&'static str>>>);
impl CatalogGet for Source {
    fn get(&self, _: &str) -> Result<String, TmdbError> {
        self.0
            .lock()
            .unwrap()
            .map(str::to_owned)
            .ok_or_else(|| TmdbError::Http("origin unavailable".into()))
    }
}

#[test]
fn cached_adopted_metadata_reads_never_require_origin_and_preserve_locale() {
    let dir = tempfile::tempdir().unwrap();
    let source = Source(Arc::new(Mutex::new(None)));
    let tmdb = Tmdb::new_at(source.clone(), &dir.path().join("catalog.db"), NOW).unwrap();
    assert!(
        tmdb.cached_details_with_meta_for(MediaKind::Movie, "603", &["zh-CN"], &["CN"])
            .unwrap()
            .is_none()
    );
    assert!(
        tmdb.cached_details_poster(MediaKind::Movie, "603")
            .unwrap()
            .is_none()
    );
    assert!(tmdb.cached_season_episodes("1396", 1).unwrap().is_none());
    assert!(
        tmdb.cached_season_details_lang("1396", 1, Some("en-US"))
            .unwrap()
            .is_none()
    );

    *source.0.lock().unwrap() = Some(include_str!("fixtures/rich_movie_details.json"));
    let expected = tmdb
        .details_with_meta_for(MediaKind::Movie, "603", &["zh-CN"], &["CN"])
        .unwrap();
    *source.0.lock().unwrap() = None;
    assert_eq!(
        tmdb.cached_details_with_meta_for(MediaKind::Movie, "603", &["zh-CN"], &["CN"])
            .unwrap(),
        Some(expected.clone())
    );
    assert!(
        tmdb.cached_details_with_meta_for(MediaKind::Movie, "603", &["en-US"], &["US"])
            .unwrap()
            .is_none()
    );
    tmdb.set_now(NOW + media::DEFAULT_TTL_SECS + 1);
    assert_eq!(
        tmdb.cached_details_with_meta_for(MediaKind::Movie, "603", &["zh-CN"], &["CN"])
            .unwrap(),
        Some(expected)
    );
}

#[test]
fn cached_artwork_and_episode_facts_preserve_successful_fields_when_offline() {
    let dir = tempfile::tempdir().unwrap();
    let source = Source(Arc::new(Mutex::new(Some(include_str!(
        "fixtures/movie_details.json"
    )))));
    let tmdb = Tmdb::new_at(source.clone(), &dir.path().join("catalog.db"), NOW).unwrap();
    let poster = tmdb.details_poster(MediaKind::Movie, "603").unwrap();
    *source.0.lock().unwrap() = None;
    assert_eq!(
        tmdb.cached_details_poster(MediaKind::Movie, "603").unwrap(),
        poster
    );
    *source.0.lock().unwrap() = Some(include_str!("fixtures/rich_season_details.json"));
    let expected = tmdb.season_episodes("1396", 1).unwrap();
    let metadata = tmdb.season_details_lang("1396", 1, Some("en-US")).unwrap();
    *source.0.lock().unwrap() = None;
    let episodes = tmdb.cached_season_episodes("1396", 1).unwrap().unwrap();
    assert_eq!(episodes.len(), expected.len());
    assert_eq!(episodes[0].episode_number, expected[0].episode_number);
    assert_eq!(episodes[0].air_date, expected[0].air_date);
    assert_eq!(
        tmdb.cached_season_details_lang("1396", 1, Some("en-US"))
            .unwrap(),
        Some(metadata)
    );
}
