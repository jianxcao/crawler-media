use super::*;

struct Offline;
impl indexer::Fetcher for Offline {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch("offline".into()))
    }
}

#[test]
fn completed_result_retries_busy_without_releasing_attempt_fence() {
    assert_completed_result_retries(false);
}

#[test]
fn completed_result_retries_sql_busy_without_releasing_attempt_fence() {
    assert_completed_result_retries(true);
}

fn assert_completed_result_retries(sql_busy: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let state = ApiState::new(
        crate::Store::open(tmp.path().join("data")).unwrap(),
        "test".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(Offline),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .unwrap();
    let job = {
        let queue = state.jobs.acquire().unwrap();
        if sql_busy {
            queue
                .with_connection(|db| db.busy_timeout(Duration::from_millis(10)))
                .unwrap();
        }
        queue
            .with_connection(|db| db.execute("UPDATE job_defs SET enabled=0", []))
            .unwrap();
        let subscribe_id = domain::SubscribeId::new();
        queue
            .enqueue(
                jobs::JobIdentity::subscribe_search(subscribe_id),
                &format!(r#"{{"subscribe_id":"{}"}}"#, subscribe_id),
                1,
            )
            .unwrap()
    };
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let held_rx = std::sync::Mutex::new(held_rx);
    let jobs = Arc::clone(&state.jobs);
    let (start_tx, start_rx) = std::sync::mpsc::channel();
    let database = tmp.path().join("data/jobs.db");
    let holder = std::thread::spawn(move || {
        start_rx.recv().unwrap();
        if sql_busy {
            let database = rusqlite::Connection::open(database).unwrap();
            database.execute_batch("BEGIN IMMEDIATE").unwrap();
            held_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            database.execute_batch("ROLLBACK").unwrap();
        } else {
            let _queue = jobs.acquire().unwrap();
            held_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        }
    });
    std::thread::scope(|scope| {
        let run = scope.spawn(|| {
            run_pending_jobs_with_clock(
                &state,
                1,
                || {
                    start_tx.send(()).unwrap();
                    held_rx.lock().unwrap().recv().unwrap();
                    2
                },
                TickSource::DevHttp,
            )
        });
        std::thread::sleep(Duration::from_millis(1400));
        assert!(
            state.active_job_attempts.lock().contains_key(&job.id),
            "executed result must retain its recovery fence while Jobs is busy"
        );
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        let finished = run.join().unwrap().unwrap();
        assert_eq!(finished.finished.len(), 1);
        assert_eq!(finished.finished[0].attempt, job.attempt);
    });
    let current = state.jobs.acquire().unwrap().get(job.id).unwrap().unwrap();
    assert_eq!(current.status, jobs::JobStatus::Succeeded);
    assert_eq!(current.attempt, job.attempt);
    assert!(state.active_job_attempts.lock().is_empty());
    assert!(
        run_pending_jobs_with_clock(&state, 10000, || 10001, TickSource::DevHttp)
            .unwrap()
            .finished
            .is_empty()
    );
}
