use domain::MediaKind;
use media::{CatalogGet, CatalogRequest, Tmdb, TmdbError};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(2);

struct Origin {
    entered: mpsc::Sender<String>,
    released: Mutex<bool>,
    ready: Condvar,
}

#[derive(Clone)]
struct Source(Arc<Origin>);

impl CatalogGet for Source {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        let _ = self.0.entered.send(path.to_owned());
        let (released, _) = self
            .0
            .ready
            .wait_timeout_while(
                self.0.released.lock().unwrap(),
                Duration::from_secs(5),
                |v| !*v,
            )
            .unwrap();
        if !*released {
            return Err(TmdbError::Http("test origin timed out".into()));
        }
        Ok(include_str!("fixtures/rich_movie_details.json").into())
    }
}

fn release(origin: &Origin) {
    *origin.released.lock().unwrap() = true;
    origin.ready.notify_all();
}

fn expired_origin_waiter_does_not_start_later() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let origin = Arc::new(Origin {
        entered: entered_tx,
        released: Mutex::new(false),
        ready: Condvar::new(),
    });
    let tmdb = Arc::new(
        Tmdb::new_at(
            Source(origin.clone()),
            &dir.path().join("cache.db"),
            1_000_000,
        )
        .unwrap(),
    );
    let workers: Vec<_> = (0..8)
        .map(|id| {
            let tmdb = tmdb.clone();
            std::thread::spawn(move || tmdb.details_poster(MediaKind::Movie, &format!("held-{id}")))
        })
        .collect();
    let entries: Vec<_> = (0..8).map(|_| entered_rx.recv_timeout(WAIT)).collect();
    let (done_tx, done_rx) = mpsc::channel();
    let target_tmdb = tmdb.clone();
    let target = std::thread::spawn(move || {
        let request = CatalogRequest::new(Instant::now() + Duration::from_millis(200));
        let result =
            request.scope(|| target_tmdb.details_poster(MediaKind::Movie, "deadline-target"));
        let _ = done_tx.send(result);
    });
    let outcome = done_rx.recv_timeout(WAIT);
    release(&origin);
    let worker_results: Vec<_> = workers.into_iter().map(|worker| worker.join()).collect();
    let target_join = target.join();
    let later_entries: Vec<_> = entered_rx.try_iter().collect();

    assert!(entries.iter().all(Result::is_ok));
    assert!(matches!(outcome, Ok(Err(TmdbError::Deadline))));
    assert!(target_join.is_ok());
    assert!(
        worker_results
            .into_iter()
            .all(|result| matches!(result, Ok(Ok(_))))
    );
    assert!(
        !later_entries
            .iter()
            .any(|path| path.contains("deadline-target"))
    );
}

#[test]
fn budgets_stop_cold_reads_and_waiters_without_cancelling_shared_work() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let origin = Arc::new(Origin {
        entered: entered_tx,
        released: Mutex::new(false),
        ready: Condvar::new(),
    });
    let tmdb = Arc::new(
        Tmdb::new_at(
            Source(origin.clone()),
            &dir.path().join("cache.db"),
            1_000_000,
        )
        .unwrap(),
    );

    let expired = CatalogRequest::new(Instant::now() - Duration::from_secs(1));
    let cold_deadline = expired.scope(|| tmdb.details_poster(MediaKind::Movie, "expired"));
    let cancelled = CatalogRequest::new(Instant::now() + WAIT);
    cancelled.cancel();
    let cold_cancelled = cancelled.scope(|| tmdb.details_poster(MediaKind::Movie, "cancelled"));

    let (done_tx, done_rx) = mpsc::channel();
    let leader_tmdb = tmdb.clone();
    let leader_tx = done_tx.clone();
    let leader = std::thread::spawn(move || {
        let result = leader_tmdb.details_poster(MediaKind::Movie, "shared");
        let _ = leader_tx.send(("leader", result));
    });
    let leader_entered = entered_rx.recv_timeout(WAIT);

    let follower_request = CatalogRequest::new(Instant::now() + WAIT);
    let follower_scope = follower_request.clone();
    let follower_tmdb = tmdb.clone();
    let follower_tx = done_tx.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let follower = std::thread::spawn(move || {
        let _ = started_tx.send(());
        let result =
            follower_scope.scope(|| follower_tmdb.details_poster(MediaKind::Movie, "shared"));
        let _ = follower_tx.send(("follower", result));
    });
    let started = started_rx.recv_timeout(WAIT);
    std::thread::sleep(Duration::from_millis(50));
    follower_request.cancel();
    let follower_result = done_rx.recv_timeout(WAIT);

    let third_tmdb = tmdb.clone();
    let third_tx = done_tx.clone();
    let third = std::thread::spawn(move || {
        let result = third_tmdb.details_poster(MediaKind::Movie, "shared");
        let _ = third_tx.send(("third", result));
    });
    release(&origin);
    let first_after_release = done_rx.recv_timeout(WAIT);
    let second_after_release = done_rx.recv_timeout(WAIT);
    let joins = [leader.join(), follower.join(), third.join()];

    assert!(matches!(cold_deadline, Err(TmdbError::Deadline)));
    assert!(matches!(cold_cancelled, Err(TmdbError::Cancelled)));
    assert!(leader_entered.is_ok());
    assert!(started.is_ok());
    assert!(matches!(
        follower_result,
        Ok(("follower", Err(TmdbError::Cancelled)))
    ));
    assert!(joins.into_iter().all(|join| join.is_ok()));
    let mut results = [first_after_release.unwrap(), second_after_release.unwrap()];
    results.sort_by_key(|(name, _)| *name);
    assert_eq!(results[0].0, "leader");
    assert_eq!(results[1].0, "third");
    assert!(results[0].1.is_ok());
    assert_eq!(
        results[0].1.as_ref().unwrap(),
        results[1].1.as_ref().unwrap()
    );
    expired_origin_waiter_does_not_start_later();
}
