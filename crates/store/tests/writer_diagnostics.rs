use store::{Db, Store, stats::StatsScope};

#[test]
fn public_stats_expose_separate_writer_wait_hold_and_covered_transactions() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (_, stats) = store.with_stats(StatsScope::Library, || {
        store
            .create_library(
                domain::MediaKind::Movie,
                "timed",
                &["/timed"],
                "everyone",
                true,
                &[],
            )
            .unwrap();
    });
    let json = serde_json::to_value(stats).unwrap();
    assert!(
        json["writer_hold_ns"].as_u64().is_some_and(|n| n > 0),
        "actual acquired mutex hold must be observable: {json}"
    );
    assert!(
        json["writer_wait_ns"].as_u64().is_some(),
        "mutex admission wait must be a separate metric"
    );
    assert!(
        json["covered_owned_write_tx_count"]
            .as_u64()
            .is_some_and(|n| n > 0),
        "write_tx-owned transactions must be counted explicitly"
    );
    assert!(
        json["covered_owned_write_tx_ns"]
            .as_u64()
            .is_some_and(|n| n > 0)
    );
    assert!(json["writer_hold_ns"].as_u64() >= json["covered_owned_write_tx_ns"].as_u64());
    store
        .writers()
        .with_writer(Db::Library, |_| Ok(()))
        .unwrap();
}

#[test]
fn observer_runs_after_writer_and_rank_release_on_success_error_and_panic() {
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
        time::Duration,
    };
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let writers = store.writers();
    let sink_writers = writers.clone();
    let samples = Rc::new(RefCell::new(Vec::new()));
    let recorded = samples.clone();
    let in_sink = Rc::new(Cell::new(false));
    store.with_writer_diagnostics(move |sample| {
        if in_sink.replace(true) { return; }
        // Same-rank reacquisition fails if the original guard/rank is still held.
        sink_writers.with_writer_timeout(Duration::ZERO, Db::Library, |db| {
            assert!(db.is_autocommit(), "cleanup must precede sink delivery");
            Ok(())
        }).unwrap();
        recorded.borrow_mut().push(serde_json::to_value(sample).unwrap());
        in_sink.set(false);
    }, || {
        writers.with_writer(Db::Library, |_| Ok(())).unwrap();
        let failed: Result<(), _> = writers.with_writer(Db::Library, |db| {
            db.execute_batch("BEGIN IMMEDIATE")?;
            Err(store::StoreError::Protected("original".into()))
        });
        assert!(matches!(failed, Err(store::StoreError::Protected(message)) if message == "original"));
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<(), _> = writers.with_writer(Db::Library, |db| {
                db.execute_batch("BEGIN IMMEDIATE")?;
                panic!("original panic");
            });
        }));
        assert_eq!(panic.unwrap_err().downcast_ref::<&str>(), Some(&"original panic"));
    });
    let samples = samples.borrow();
    assert_eq!(samples.len(), 3);
    assert_eq!(
        samples
            .iter()
            .map(|sample| sample["outcome"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ok", "error", "panic"]
    );
    for sample in samples.iter() {
        assert_eq!(sample["database"], "Library");
        assert!(sample["writer_hold_ns"].as_u64().unwrap() > 0);
        assert_eq!(
            sample["covered_owned_write_tx_count"], 0,
            "raw transactions are deliberately outside coverage"
        );
    }
}

#[test]
fn diagnostic_delivery_suppresses_recursion_and_preserves_business_errors() {
    use std::{cell::Cell, rc::Rc};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let writers = store.writers();
    let sink = writers.clone();
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    store.with_writer_diagnostics(
        move |_| {
            observed.set(observed.get() + 1);
            if observed.get() < 3 {
                sink.with_writer(Db::Library, |_| Ok(())).unwrap();
            }
        },
        || writers.with_writer(Db::Library, |_| Ok(())).unwrap(),
    );
    assert_eq!(
        calls.get(),
        1,
        "observer writes must not recursively deliver telemetry"
    );
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.with_writer_diagnostics(
            |_| panic!("diagnostic sink failure"),
            || {
                writers.with_writer::<()>(Db::Library, |_| {
                    Err(store::StoreError::Protected("business error".into()))
                })
            },
        )
    }));
    assert!(
        matches!(result, Ok(Err(store::StoreError::Protected(message))) if message == "business error")
    );
}

#[test]
fn rejected_nested_database_sample_waits_until_outer_writer_releases() {
    use std::{cell::Cell, rc::Rc, time::Duration};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let writers = store.writers();
    let sink = writers.clone();
    let calls = Rc::new(Cell::new(0));
    let recorded = calls.clone();
    store.with_writer_diagnostics(
        move |_| {
            sink.with_writer_timeout(Duration::ZERO, Db::App, |_| Ok(()))
                .unwrap();
            recorded.set(recorded.get() + 1);
        },
        || {
            writers
                .with_writer(Db::App, |_| {
                    assert!(matches!(
                        writers.with_writer(Db::Library, |_| Ok(())),
                        Err(store::StoreError::LockOrder { .. })
                    ));
                    assert_eq!(
                        calls.get(),
                        0,
                        "sink must wait until the outer writer releases"
                    );
                    Ok(())
                })
                .unwrap();
        },
    );
    assert_eq!(calls.get(), 2);
}

#[test]
fn rejected_writer_under_jobs_rank_delivers_only_after_jobs_release() {
    use std::{cell::Cell, rc::Rc, time::Duration};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let writers = store.writers();
    let sink = writers.clone();
    let delivered = Rc::new(Cell::new(0));
    let observed = delivered.clone();
    store.with_writer_diagnostics(
        move |_| {
            sink.with_writer_timeout(Duration::ZERO, Db::Library, |_| Ok(()))
                .unwrap();
            observed.set(observed.get() + 1);
        },
        || {
            let jobs = store::WriterRankGuard::enter(store::WriterRank::Jobs).unwrap();
            assert!(matches!(
                writers.with_writer(Db::Library, |_| Ok(())),
                Err(store::StoreError::LockOrder { .. })
            ));
            assert_eq!(delivered.get(), 0);
            drop(jobs);
            assert_eq!(
                delivered.get(),
                1,
                "Jobs release must drain without requiring a later Store write"
            );
        },
    );
}

#[test]
fn deferred_samples_retain_observers_registered_when_the_attempt_was_made() {
    use std::{cell::Cell, rc::Rc};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let writers = store.writers();
    let inner_calls = Rc::new(Cell::new(0));
    let outer_calls = Rc::new(Cell::new(0));
    let outer = outer_calls.clone();
    store.with_writer_diagnostics(
        move |_| outer.set(outer.get() + 1),
        || {
            writers
                .with_writer(Db::App, |_| {
                    let inner = inner_calls.clone();
                    store.with_writer_diagnostics(
                        move |_| inner.set(inner.get() + 1),
                        || {
                            assert!(matches!(
                                writers.with_writer(Db::Cache, |_| Ok(())),
                                Err(store::StoreError::LockOrder { .. })
                            ));
                        },
                    );
                    assert_eq!(
                        inner_calls.get(),
                        0,
                        "the scope may exit before safe delivery"
                    );
                    Ok(())
                })
                .unwrap();
        },
    );
    assert_eq!(
        inner_calls.get(),
        1,
        "inner observer must retain its deferred attempt"
    );
    assert_eq!(
        outer_calls.get(),
        2,
        "outer observes rejected attempt and successful outer write"
    );
}
#[test]
fn owned_write_transactions_measure_commit_error_and_panic_without_nested_double_count() {
    use domain::{Media, MediaId, MediaKind};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "owned-timing".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    for mode in ["commit", "error", "panic"] {
        let mut item = media.clone();
        item.id = MediaId::new();
        let (outcome, stats) = store.with_stats(StatsScope::Library, || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                store.library_write(|db| {
                    db.insert_media(&item)?;
                    assert_eq!(db.get_media(item.id)?, Some(item.clone()));
                    match mode {
                        "error" => Err(store::StoreError::Protected("owned refusal".into())),
                        "panic" => panic!("owned panic"),
                        _ => Ok(()),
                    }
                })
            }))
        });
        assert_eq!(
            stats.covered_owned_write_tx_count, 1,
            "only outer owned transaction counted: {mode}"
        );
        assert!(stats.covered_owned_write_tx_ns > 0);
        assert!(stats.writer_hold_ns >= stats.covered_owned_write_tx_ns);
        assert_eq!(
            store.get_media(item.id).unwrap().is_some(),
            mode == "commit"
        );
        if mode == "commit" {
            assert!(matches!(outcome, Ok(Ok(()))));
        } else if mode == "error" {
            assert!(matches!(outcome, Ok(Err(store::StoreError::Protected(_)))));
        } else {
            assert!(outcome.is_err());
        }
    }
}

#[test]
fn peer_mutex_contention_and_refused_admission_have_independent_timing_samples() {
    use std::{cell::RefCell, rc::Rc, sync::mpsc, time::Duration};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let writers = store.writers();
    let peer = writers.clone();
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        peer.with_writer(Db::Library, |_| {
            held_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        })
        .unwrap()
    });
    held_rx.recv().unwrap();
    let samples = Rc::new(RefCell::new(Vec::new()));
    let recorded = samples.clone();
    store.with_writer_diagnostics(
        move |sample| {
            recorded
                .borrow_mut()
                .push(serde_json::to_value(sample).unwrap())
        },
        || {
            assert!(matches!(
                writers.with_writer_timeout(Duration::ZERO, Db::Library, |_| Ok(())),
                Err(store::StoreError::WriteBusy)
            ));
            release_tx.send(()).unwrap();
            thread.join().unwrap();
            let refused = writers.with_write_admission(
                Duration::from_secs(1),
                || Err(store::StoreError::Protected("admission fixture".into())),
                || {
                    writers.with_writer::<()>(Db::Library, |_| {
                        panic!("refused writer work must not run")
                    })
                },
            );
            assert!(refused.is_err());
        },
    );
    let samples = samples.borrow();
    assert_eq!(samples.len(), 2);
    assert_eq!(samples[0]["outcome"], "busy");
    assert_eq!(samples[0]["writer_hold_ns"], 0);
    assert!(samples[0]["writer_wait_ns"].as_u64().unwrap() > 0);
    assert_eq!(samples[1]["outcome"], "error");
    assert!(samples[1]["writer_hold_ns"].as_u64().unwrap() > 0);
    assert_eq!(samples[1]["covered_owned_write_tx_count"], 0);
}
