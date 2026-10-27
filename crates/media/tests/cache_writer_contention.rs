//! C03: concurrent first opens of one cache file must not lose schema or data.

use std::sync::{Arc, Barrier};

use media::response_cache::{self, CacheSource};

#[test]
fn concurrent_first_open_and_put_keep_schema_and_data() {
    for round in 0..20 {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("catalog.db");
        let barrier = Arc::new(Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|index| {
                let (path, barrier) = (path.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    let conn = response_cache::open(&path)?;
                    response_cache::put(
                        &conn,
                        CacheSource::Tmdb,
                        &format!("key-{index}"),
                        &format!("value-{index}"),
                        1,
                        None,
                    )
                })
            })
            .collect();
        for (index, worker) in workers.into_iter().enumerate() {
            let outcome = worker.join().unwrap();
            assert!(outcome.is_ok(), "round {round} worker {index}: {outcome:?}");
        }
        let conn = response_cache::open(&path).unwrap();
        for index in 0..8 {
            let committed_cache_value =
                response_cache::get(&conn, CacheSource::Tmdb, &format!("key-{index}"))
                    .unwrap()
                    .map(|row| row.body);
            assert_eq!(committed_cache_value, Some(format!("value-{index}")));
        }
    }
}
