use api::{ApiState, Store, router};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use domain::{Filter, FilterId, Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};
use tower::ServiceExt;

#[path = "../support/writer_hold.rs"]
mod writer_hold;

const TOKEN: &str = "coverage-atomicity-fixture";
struct Offline;
impl Fetcher for Offline {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline coverage fixture".into()))
    }
}

pub struct Fixture {
    _temp: tempfile::TempDir,
    pub state: ApiState,
    pub media: MediaId,
    filter: FilterId,
    library: String,
}
impl Fixture {
    pub fn new() -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let store = Store::open(temp.path().join("data")).unwrap();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Coverage atomicity".into(),
            year: Some(2026),
            original_title: None,
            aliases: vec![],
            tmdb_id: Some("coverage-atomicity".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        let filter = Filter::new(FilterId::new(), "Coverage filter", vec![]);
        store.insert_filter(&filter).unwrap();
        let library = store.default_library(MediaKind::Tv).unwrap().unwrap().id;
        let state = ApiState::new(
            store,
            TOKEN.into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(Offline),
            Arc::new(MemoryDownloader::new(temp.path().join("stage"))),
            temp.path().join("library"),
        )
        .unwrap();
        Self {
            _temp: temp,
            state,
            media: media.id,
            filter: filter.id,
            library,
        }
    }
    pub fn body(&self, from: u32, to: u32) -> Value {
        json!({
            "media": { "kind": "tv", "title": "Coverage atomicity", "year": 2026,
                       "tmdb_id": "coverage-atomicity" },
            "coverage": coverage(from, to), "fetch_mode": "search",
            "filter_id": self.filter.to_string(), "library_id": self.library,
        })
    }
    pub async fn call(&self, method: &'static str, uri: &str, body: Value) -> (StatusCode, Value) {
        call(router(self.state.clone()), method, uri.to_string(), body).await
    }
    pub async fn concurrent(
        &self,
        method: &'static str,
        uris: [String; 2],
        bodies: [Value; 2],
        patch: bool,
    ) -> [(StatusCode, Value); 2] {
        let pool = self.state.store().reader_pool(store::Db::Subscribe);
        let writer = writer_hold::WriterHold::acquire(self.state.store(), store::Db::Subscribe);
        let gate = Arc::new(ReadGate::default());
        let observer = gate.clone();
        pool.set_query_progress_observer(Some(Arc::new(move || observer.observe(patch))));
        let [first_uri, second_uri] = uris;
        let [first_body, second_body] = bodies;
        let first = tokio::spawn(call(
            router(self.state.clone()),
            method,
            first_uri,
            first_body,
        ));
        let second = tokio::spawn(call(
            router(self.state.clone()),
            method,
            second_uri,
            second_body,
        ));
        // The writer barrier creates a race window for implementations that read
        // before mutation. A fixed writer-owned check need not use these readers.
        let until = Instant::now() + Duration::from_millis(500);
        while gate.waiting() < 2 && Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let observed_both = gate.waiting() == 2;
        gate.release();
        if observed_both {
            let until = Instant::now() + Duration::from_millis(200);
            while (pool.available() != 2 || pool.waiting() != 0) && Instant::now() < until {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }
        pool.set_query_progress_observer(None);
        drop(writer);
        [first.await.unwrap(), second.await.unwrap()]
    }
    pub fn search_job_subscribes(&self) -> Vec<String> {
        self.state
            .jobs()
            .lock()
            .list_defs()
            .unwrap()
            .into_iter()
            .filter(|def| def.kind == jobs::JobKind::SubscribeSearch)
            .map(|def| {
                serde_json::from_str::<Value>(&def.payload).unwrap()["subscribe_id"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }
}

pub fn coverage(from: u32, to: u32) -> Value {
    json!({ "kind": "tv", "season": 1, "episode_from": from, "episode_to": to })
}
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}
async fn call(
    app: axum::Router,
    method: &'static str,
    uri: String,
    body: Value,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(4), app.oneshot(request))
        .await
        .expect("finite coverage mutation must finish")
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[derive(Default)]
struct ReadGate {
    state: Mutex<(usize, bool)>,
    reads: Mutex<HashMap<std::thread::ThreadId, usize>>,
    changed: Condvar,
}
impl ReadGate {
    fn observe(&self, patch: bool) {
        let read = {
            let mut reads = self.reads.lock().unwrap();
            let read = reads.entry(std::thread::current().id()).or_default();
            *read += 1;
            *read
        };
        // PATCH first loads its own Subscribe, then checks siblings. Scheduling
        // only pauses the latter; assertions below observe HTTP and durable facts.
        if read != if patch { 2 } else { 1 } {
            return;
        }
        let mut state = self.state.lock().unwrap();
        if state.0 >= 2 {
            return;
        }
        state.0 += 1;
        let (next, _) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(2), |state| !state.1)
            .unwrap();
        state = next;
        assert!(state.1, "controller must release the overlap observation");
    }
    fn waiting(&self) -> usize {
        self.state.lock().unwrap().0
    }
    fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.changed.notify_all();
    }
}
impl Drop for ReadGate {
    fn drop(&mut self) {
        self.release();
    }
}
