use std::sync::{Arc, Mutex};

use domain::MediaKind;
use media::{CatalogGet, Tmdb, TmdbError};

struct AggregateCreditsFixture {
    requests: Arc<Mutex<Vec<String>>>,
}

impl CatalogGet for AggregateCreditsFixture {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        self.requests.lock().unwrap().push(path.to_string());
        if path.starts_with(
            "/tv/294990?append_to_response=aggregate_credits,content_ratings,translations",
        ) {
            Ok(include_str!("fixtures/tv_aggregate_credits.json").to_string())
        } else {
            Err(TmdbError::Http(format!("unexpected TMDB path: {path}")))
        }
    }
}

#[test]
fn tv_metadata_fetches_and_parses_the_full_aggregate_cast() {
    let dir = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let tmdb = Tmdb::new(
        AggregateCreditsFixture {
            requests: requests.clone(),
        },
        &dir.path().join("catalog.db"),
    )
    .unwrap();

    let metadata = tmdb.details_with_meta(MediaKind::Tv, "294990").unwrap();

    assert_eq!(metadata.cast.len(), 33);
    assert_eq!(metadata.cast[0].name, "许凯");
    assert_eq!(metadata.cast[0].role.as_deref(), Some("Shen Run / Yan Rui"));
    assert_eq!(metadata.cast[0].person_id, Some(2_091_759));
    assert_eq!(
        metadata.cast[0].avatar_path.as_deref(),
        Some("/wEfatQWa3v9tNtmgr5LOOmFSSJj.jpg")
    );
    assert_eq!(metadata.cast[2].name, "闵春晓");
    assert_eq!(
        requests.lock().unwrap().as_slice(),
        [
            "/tv/294990?append_to_response=aggregate_credits,content_ratings,translations&language=zh-CN"
        ]
    );
}
