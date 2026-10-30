//! Production catalog/Fanart storage shares the Cache writer and readonly pool.
use super::support::*;
use media::{CatalogGet, TmdbError};
use std::sync::{Arc, mpsc};
use std::time::Duration;

struct FakeCatalogHttp(mpsc::Sender<String>);
impl CatalogGet for FakeCatalogHttp {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        self.0.send(path.into()).unwrap();
        Ok(r#"{"results":[],"hdmovielogo":[{"url":"https://art.example/logo.png","lang":"en","likes":"1"}]}"#.into())
    }
}

type Fetch = Arc<dyn Fn(&str) -> Result<(), TmdbError> + Send + Sync>;

fn assert_shared_cache(fanart: bool) {
    let fixture = Fixture::idle();
    let (http_entered, http_calls) = mpsc::channel();
    let source = if fanart {
        media::response_cache::CacheSource::Fanart
    } else {
        media::response_cache::CacheSource::Tmdb
    };
    let cache = fixture.state.catalog_cache(source);
    let fetch: Fetch = if fanart {
        let client = media::fanart::FanartClient::with_cache(
            FakeCatalogHttp(http_entered),
            Arc::new(cache),
            "fixture-api-key",
        );
        Arc::new(move |key| client.movie(key, &["en"]).map(|_| ()))
    } else {
        let client = media::Tmdb::with_cache(FakeCatalogHttp(http_entered), cache);
        Arc::new(move |key| client.search_movie(key).map(|_| ()))
    };
    fetch("cached").unwrap();
    http_calls.recv_timeout(Duration::from_secs(1)).unwrap();

    let gate = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(gate.clone());
    let store = fixture.state.store();
    let held = gate.clone();
    let writer = std::thread::spawn(move || {
        store.with_writer(store::Db::Cache, |_| {
            held.block();
            Ok(())
        })
    });
    runtime().block_on(gate.entered());

    let (refresh_done, refresh_result) = mpsc::channel();
    let refresh = fetch.clone();
    let refreshing = std::thread::spawn(move || refresh_done.send(refresh("uncached")).unwrap());
    http_calls
        .recv_timeout(Duration::from_secs(1))
        .expect("cache miss must fetch outside Cache writer");
    let early_write = refresh_result.recv_timeout(SLO);

    let (hit_done, hit_result) = mpsc::channel();
    let hit = fetch.clone();
    let reading = std::thread::spawn(move || hit_done.send(hit("cached")).unwrap());
    let cached = hit_result.recv_timeout(SLO);
    gate.release();
    writer.join().unwrap().unwrap();
    refreshing.join().unwrap();
    reading.join().unwrap();
    assert!(
        early_write.is_err(),
        "cache miss must queue behind the application's only Cache writer"
    );
    cached
        .expect("fresh cache hits must use readonly pool while Cache writer is held")
        .unwrap();
    refresh_result
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    assert!(
        http_calls.try_recv().is_err(),
        "fresh cache hit must not access HTTP"
    );
}

#[test]
fn catalog_refresh_uses_shared_writer_and_fresh_reads_use_pool() {
    assert_shared_cache(false);
}

#[test]
fn fanart_refresh_uses_shared_writer_and_fresh_reads_use_pool() {
    assert_shared_cache(true);
}
