use std::sync::Arc;

use api::{ApiState, Store};
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

#[tokio::test]
async fn independently_created_queue_reopens_through_api_and_retains_inflight_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let queue = jobs::Queue::open(data.join("jobs.db")).unwrap();
    let job = queue.enqueue(jobs::JobKind::Transfer, "keep", 10).unwrap();
    let claimed = queue.claim(10, 1).unwrap().pop().unwrap();
    assert_eq!(claimed.id, job.id);
    drop(queue);
    let state = ApiState::new(
        Store::open(&data).unwrap(),
        "secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
        tmp.path().join("library"),
    )
    .unwrap();
    assert_eq!(state.jobs.lock().get(job.id).unwrap(), Some(claimed));
}
