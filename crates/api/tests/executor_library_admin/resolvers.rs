use super::*;

#[test]
fn cached_marker_resolver_waits_for_store_off_executor() {
    let f = Fixture::new();
    let coverage = {
        let handle = f.state.store();
        let store = handle.as_ref();
        store
            .put_cached_chapters(&f.file.file_id.to_string(), &[])
            .unwrap();
        store.file_coverage_by_id(f.file.file_id).unwrap().unwrap()
    };
    let media = f.state.store().get_media(f.file.media_id).unwrap().unwrap();
    let handle = f.state.store();
    let (tx, rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = crate::writer_hold::WriterHold::acquire(handle.clone(), store::Db::Library);
        tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(500));
    });
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (chapters, elapsed) = runtime().block_on(async {
        let started = Instant::now();
        tokio::join!(
            api::marker_resolver::resolve_item_chapters(
                &f.state,
                &coverage,
                domain::UnitKey::WHOLE,
                &media,
                None
            ),
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                started.elapsed()
            }
        )
    });
    holder.join().unwrap();
    assert!(chapters.is_empty());
    assert!(
        elapsed < Duration::from_millis(200),
        "marker resolver parked timer for {elapsed:?}"
    );
}

struct ContendedCoverRenderer {
    store: Arc<Store>,
    entered: parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<Instant>>>,
    holders: Arc<parking_lot::Mutex<Vec<std::thread::JoinHandle<()>>>>,
}

impl api::library_cover_service::CoverRenderer for ContendedCoverRenderer {
    fn render(
        &self,
        _: &store::Library,
        _: &[domain::LibraryFile],
        _: &cover_generator::CoverOptions,
    ) -> Result<Option<Vec<u8>>, String> {
        let store = self.store.clone();
        let entered = self.entered.lock().take().unwrap();
        let (locked, acquired) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _guard = crate::writer_hold::WriterHold::acquire(store.clone(), store::Db::Library);
            locked.send(()).unwrap();
            let _ = entered.send(Instant::now());
            std::thread::sleep(Duration::from_millis(500));
        });
        acquired.recv_timeout(Duration::from_secs(2)).unwrap();
        self.holders.lock().push(holder);
        Ok(Some(b"cover bytes".to_vec()))
    }
}

#[test]
fn explicit_library_cover_cas_keeps_executor_responsive_after_rendering() {
    let mut f = Fixture::new();
    let (entered, started) = tokio::sync::oneshot::channel();
    let holders = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let renderer = ContendedCoverRenderer {
        store: f.state.store(),
        entered: parking_lot::Mutex::new(Some(entered)),
        holders: holders.clone(),
    };
    f.state = f.state.with_cover_service(Arc::new(
        api::library_cover_service::LibraryCoverService::new(Arc::new(renderer)),
    ));
    let uri = format!("/api/v1/libraries/{}/cover/generate", f.file.library_id);
    let (response, elapsed) = runtime().block_on(async {
        tokio::join!(
            router(f.state.clone()).oneshot(
                Request::post(uri)
                    .header("authorization", format!("Bearer {}", f.token))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap()
            ),
            async {
                let start = tokio::time::timeout(Duration::from_secs(3), started)
                    .await
                    .unwrap()
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(10)).await;
                start.elapsed()
            }
        )
    });
    for holder in std::mem::take(&mut *holders.lock()) {
        holder.join().unwrap();
    }
    assert_eq!(response.unwrap().status(), StatusCode::OK);
    assert!(
        elapsed < Duration::from_millis(200),
        "cover CAS parked timer for {elapsed:?}"
    );
}

struct EmptyAfterManual {
    entered: parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: parking_lot::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl api::library_cover_service::CoverRenderer for EmptyAfterManual {
    fn render(
        &self,
        _: &store::Library,
        _: &[domain::LibraryFile],
        _: &cover_generator::CoverOptions,
    ) -> Result<Option<Vec<u8>>, String> {
        let _ = self.entered.lock().take().unwrap().send(());
        self.release
            .lock()
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        Ok(None)
    }
}

#[test]
fn late_empty_auto_render_does_not_change_a_newer_manual_cover_marker() {
    let mut f = Fixture::new();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let service = Arc::new(api::library_cover_service::LibraryCoverService::new(
        Arc::new(EmptyAfterManual {
            entered: parking_lot::Mutex::new(Some(entered)),
            release: parking_lot::Mutex::new(released),
        }),
    ));
    f.state = f.state.with_cover_service(service);
    runtime().block_on(async {
        let worker = f.state.clone();
        let id = f.file.library_id.to_string();
        let worker_id = id.clone();
        let auto = tokio::spawn(async move {
            worker
                .cover_service()
                .ensure_cover(&worker, &worker_id)
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), started)
            .await
            .unwrap()
            .unwrap();
        let manual = f
            .state
            .cover_service()
            .replace_cover(&f.state, &id, b"manual".to_vec())
            .await;
        release.send(()).unwrap();
        let manual = manual.unwrap();
        assert!(auto.await.unwrap().unwrap().is_none());
        let handle = f.state.store();
        let store = handle.as_ref();
        assert_eq!(
            store.get_library(&id).unwrap().unwrap().cover_path,
            Some(manual.display().to_string())
        );
        assert_eq!(
            store
                .get_setting(&format!("library_cover_checked.{id}"))
                .unwrap()
                .as_deref(),
            Some("manual")
        );
    });
}

#[test]
fn marker_resolver_does_not_return_a_new_sources_cache_for_a_stale_file_snapshot() {
    let f = Fixture::new();
    let stale = f
        .state
        .store()
        .file_coverage_by_id(f.file.file_id)
        .unwrap()
        .unwrap();
    std::fs::write(&f.file.path, b"new source bytes").unwrap();
    let observed = library::observe_source(&f.file.path).unwrap();
    let handle = f.state.store();
    let store = handle.as_ref();
    let library = f.file.library_id.to_string();
    store
        .replace_file_source(
            &store::library_write::WriteScope {
                library_id: f.file.library_id,
                config_revision: store.library_config_revision(&library).unwrap(),
            },
            f.file.file_id,
            &f.file.version,
            &observed,
            1000,
        )
        .unwrap();
    store
        .put_cached_chapters(
            &f.file.file_id.to_string(),
            &[marker::ChapterMarker {
                start_ms: 10,
                end_ms: 20,
                title: Some("new source".into()),
                marker_type: None,
                synthetic: false,
            }],
        )
        .unwrap();
    let media = store.get_media(f.file.media_id).unwrap().unwrap();
    let chapters = runtime().block_on(api::marker_resolver::resolve_item_chapters(
        &f.state,
        &stale,
        domain::UnitKey::WHOLE,
        &media,
        None,
    ));
    assert!(
        chapters.is_empty(),
        "stale snapshot returned a new source's chapter cache"
    );
    let refreshed = runtime().block_on(api::marker_resolver::force_refresh_item_chapters(
        &f.state,
        &stale,
        domain::UnitKey::WHOLE,
        &media,
        None,
    ));
    assert!(matches!(
        refreshed,
        Err(api::marker_resolver::ChapterRefreshError::SourceChanged(_))
    ));
    let handle = f.state.store();
    let store = handle.as_ref();
    let retained = store
        .get_cached_chapters(&f.file.file_id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(retained[0].title.as_deref(), Some("new source"));
}
