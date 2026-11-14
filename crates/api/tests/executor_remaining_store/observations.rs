use super::*;

struct SlowFiles {
    entered: std::sync::mpsc::Sender<()>,
}

impl api::file_stat_read::FileStatRead for SlowFiles {
    fn read_nfo(&self, _: &std::path::Path) -> Option<library::NfoMeta> {
        None
    }
    fn metadata(&self, path: &std::path::Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }
    fn is_file(&self, path: &std::path::Path) -> bool {
        let _ = self.entered.send(());
        std::thread::sleep(Duration::from_millis(300));
        path.is_file()
    }
}

#[test]
fn collection_artwork_observation_does_not_hold_store_or_executor() {
    for suffix in ["list", "/items", "/gallery"] {
        let mut f = Fixture::new();
        let (tx, rx) = std::sync::mpsc::channel();
        f.state = f
            .state
            .with_file_stat_read(Arc::new(SlowFiles { entered: tx }));
        let uri = if suffix == "list" {
            "/api/v1/collections".into()
        } else {
            f.uri(suffix)
        };
        let app = router(f.state.clone());
        let store = f.state.store();
        let observer = std::thread::spawn(move || {
            rx.recv_timeout(Duration::from_secs(2))
                .expect("file seam must be used");
            let before = Instant::now();
            store
                .writers()
                .with_writer_timeout(Duration::from_millis(100), store::Db::Library, |_| Ok(()))
                .expect("slow artwork holds Library writer");
            let guard = store.as_ref();
            assert!(!guard.list_libraries().unwrap().is_empty());
            before.elapsed()
        });
        let (response, elapsed) = runtime().block_on(async {
            let started = Instant::now();
            tokio::join!(
                app.oneshot(request("GET", &uri, &f.token, Value::Null)),
                async {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    started.elapsed()
                }
            )
        });
        let observed = observer.join().unwrap();
        assert_eq!(response.unwrap().status(), StatusCode::OK);
        assert!(observed < Duration::from_millis(200));
        assert!(
            elapsed < Duration::from_millis(200),
            "artwork parked executor: {elapsed:?}"
        );
    }
}
