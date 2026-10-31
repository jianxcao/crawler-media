use super::*;

fn seed_subscribe(f: &Fixture) -> domain::SubscribeId {
    let handle = f.state.store();
    let store = handle.as_ref();
    let id = domain::SubscribeId::new();
    store
        .insert_subscribe(&domain::Subscribe {
            id,
            user_id: f.user,
            media_id: f.media,
            coverage: domain::Coverage::Movie,
            fetch_mode: domain::FetchMode::Search,
            filter_id: store.list_filters().unwrap()[0].id,
            wash_cut: false,
            wash_cut_filter_id: None,
            keep_old_versions: false,
            full_season_pack: false,
            downloader_id: None,
            library_id: None,
            tracking_state: "active".into(),
            follow_future: false,
            search_interval_secs: 1800,
        })
        .unwrap();
    f.state
        .jobs()
        .lock()
        .ensure_def(jobs::NewDef {
            kind: jobs::JobKind::SubscribeSearch,
            name: format!("search {id}"),
            enabled: true,
            schedule: Some(jobs::Schedule::Interval { secs: 1800 }),
            payload: json!({"subscribe_id":id}).to_string(),
            timeout_secs: Some(120),
            concurrency_key: Some(format!("subscribe:{id}")),
        })
        .unwrap();
    id
}

#[test]
fn subscription_detail_and_depth_routes_keep_executor_responsive() {
    for route in 0..8 {
        let f = Fixture::new();
        let id = seed_subscribe(&f);
        let base = format!("/api/v1/subscriptions/{id}");
        let (method, uri, body) = match route {
            0 => ("GET", base, Value::Null),
            1 => ("GET", format!("{base}/release-forecast"), Value::Null),
            2 => ("GET", format!("{base}/activities"), Value::Null),
            3 => ("GET", format!("{base}/removal-preview"), Value::Null),
            4 => (
                "POST",
                format!("{base}/upgrade-runs"),
                json!({"wash_cut":false}),
            ),
            5 => (
                "POST",
                format!("{base}/season-cleanup"),
                json!({"seasons":[1]}),
            ),
            6 => (
                "GET",
                "/api/v1/subscriptions/today-arrivals".into(),
                Value::Null,
            ),
            _ => (
                "GET",
                "/api/v1/subscriptions/automation-readiness".into(),
                Value::Null,
            ),
        };
        let app = router(f.state.clone());
        let holder = hold_store(&f.state, store::Db::Subscribe, Duration::from_millis(500));
        let (response, elapsed) = runtime().block_on(async {
            let started = Instant::now();
            tokio::join!(app.oneshot(request(method, &uri, &f.token, body)), async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                started.elapsed()
            })
        });
        holder.join().unwrap();
        let response = response.unwrap();
        let status = response.status();
        let body = runtime().block_on(json_response(response));
        assert_eq!(status, StatusCode::OK, "{method} {uri}: {body}");
        assert!(
            elapsed < Duration::from_millis(200),
            "{method} {uri} parked timer: {elapsed:?}"
        );
    }
}

struct SlowCachedCatalog(Arc<Store>);
impl api::catalog::Catalog for SlowCachedCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
    fn cached_season_episodes(
        &self,
        _: &str,
        _: u32,
    ) -> Result<Option<Vec<media::SeasonEpisode>>, String> {
        assert!(
            self.0
                .writers()
                .with_writer_timeout(Duration::from_millis(100), store::Db::Library, |_| Ok(()))
                .is_ok(),
            "Cached catalog called under Store writer"
        );
        std::thread::sleep(Duration::from_millis(500));
        Ok(Some(vec![media::SeasonEpisode {
            episode_number: 1,
            air_date: Some("2024-01-01".into()),
        }]))
    }
}

#[test]
fn subscription_detail_cached_catalog_is_off_executor_and_outside_store() {
    let mut f = Fixture::new();
    let id = seed_subscribe(&f);
    let id = {
        let handle = f.state.store();
        let store = handle.as_ref();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "TV fixture".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: Some("100".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        let mut subscription = store.get_subscribe(id).unwrap().unwrap();
        subscription.media_id = media.id;
        subscription.coverage = domain::Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: Some(2),
        };
        subscription.id = domain::SubscribeId::new();
        store.insert_subscribe(&subscription).unwrap();
        subscription.id
    };
    f.state = f
        .state
        .clone()
        .with_catalog(Arc::new(SlowCachedCatalog(f.state.store())));
    let (response, elapsed) = runtime().block_on(async {
        let started = Instant::now();
        tokio::join!(
            router(f.state.clone()).oneshot(request(
                "GET",
                &format!("/api/v1/subscriptions/{id}"),
                &f.token,
                Value::Null,
            )),
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                started.elapsed()
            }
        )
    });
    let response = response.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = runtime().block_on(json_response(response));
    assert_eq!(body["data"]["wanted"][0]["air_date"], "2024-01-01");
    assert!(
        elapsed < Duration::from_millis(200),
        "Catalog parked executor: {elapsed:?}"
    );
}

struct SlowPoster(Arc<Store>);
impl api::file_stat_read::FileStatRead for SlowPoster {
    fn read_nfo(&self, _: &std::path::Path) -> Option<library::NfoMeta> {
        None
    }
    fn metadata(&self, path: &std::path::Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }
    fn is_file(&self, path: &std::path::Path) -> bool {
        assert!(
            self.0
                .writers()
                .with_writer_timeout(Duration::from_millis(100), store::Db::Library, |_| Ok(()))
                .is_ok(),
            "poster observed under Store"
        );
        std::thread::sleep(Duration::from_millis(300));
        path.is_file()
    }
}

#[test]
fn subscription_list_detail_create_and_patch_observe_posters_outside_store() {
    for route in 0..4 {
        let mut f = Fixture::new();
        f.state
            .store()
            .add_media_alias(
                f.media,
                &domain::MediaAlias {
                    source: "tmdb".into(),
                    kind: MediaKind::Movie,
                    external_id: "123".into(),
                },
            )
            .unwrap();
        let id = if route == 3 {
            domain::SubscribeId::new()
        } else {
            seed_subscribe(&f)
        };
        f.state = f
            .state
            .clone()
            .with_file_stat_read(Arc::new(SlowPoster(f.state.store())));
        let base = format!("/api/v1/subscriptions/{id}");
        let (method, uri, body, status) = match route {
            0 => (
                "GET",
                "/api/v1/subscriptions".to_string(),
                Value::Null,
                StatusCode::OK,
            ),
            1 => ("GET", base, Value::Null, StatusCode::OK),
            2 => ("PATCH", base, json!({"wash_cut":true}), StatusCode::OK),
            _ => (
                "POST",
                "/api/v1/subscriptions".into(),
                json!({"media":{"kind":"movie","title":"Collection fixture","tmdb_id":"123"},"coverage":{"kind":"movie"}}),
                StatusCode::CREATED,
            ),
        };
        let (response, elapsed) = runtime().block_on(async {
            let started = Instant::now();
            tokio::join!(
                router(f.state.clone()).oneshot(request(method, &uri, &f.token, body)),
                async {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    started.elapsed()
                }
            )
        });
        let response = response.unwrap();
        assert_eq!(response.status(), status, "{method} {uri}");
        let body = runtime().block_on(json_response(response));
        let data = if route == 0 {
            &body["data"][0]
        } else {
            &body["data"]
        };
        assert!(
            data["media"]["poster_url"]
                .as_str()
                .unwrap()
                .starts_with("/posters/")
        );
        assert!(
            elapsed < Duration::from_millis(200),
            "{method} {uri}: {elapsed:?}"
        );
    }
}

struct SlowDownloader(std::sync::OnceLock<Arc<Store>>);
impl downloader::Downloader for SlowDownloader {
    fn add(&self, _: &domain::Torrent) -> Result<(), downloader::DownloaderError> {
        Ok(())
    }
    fn completed_files(
        &self,
        _: &domain::Torrent,
    ) -> Result<Vec<std::path::PathBuf>, downloader::DownloaderError> {
        Ok(vec![])
    }
    fn task_snapshots(&self) -> Result<Vec<downloader::TaskSnapshot>, downloader::DownloaderError> {
        assert!(
            self.0
                .get()
                .unwrap()
                .writers()
                .with_writer_timeout(Duration::from_millis(100), store::Db::Subscribe, |_| Ok(()))
                .is_ok(),
            "Downloader under Store"
        );
        std::thread::sleep(Duration::from_millis(500));
        Ok(vec![])
    }
}

#[test]
fn subscription_removal_preview_downloader_is_off_executor_and_outside_store() {
    let mut f = Fixture::new();
    let media = f.state.store().get_media(f.media).unwrap().unwrap();
    let downloader = Arc::new(SlowDownloader(std::sync::OnceLock::new()));
    f.state = ApiState::new(
        Store::open(f._tmp.path().join("preview-data")).unwrap(),
        f.token.clone(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        downloader.clone(),
        f._tmp.path().join("preview-library"),
    )
    .unwrap();
    assert!(downloader.0.set(f.state.store()).is_ok());
    f.user = f.state.store().user_id_by_token(&f.token).unwrap().unwrap();
    f.state.store().insert_media(&media).unwrap();
    let id = seed_subscribe(&f);
    let (response, elapsed) = runtime().block_on(async {
        let started = Instant::now();
        tokio::join!(
            router(f.state.clone()).oneshot(request(
                "GET",
                &format!("/api/v1/subscriptions/{id}/removal-preview"),
                &f.token,
                Value::Null
            )),
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                started.elapsed()
            }
        )
    });
    assert_eq!(response.unwrap().status(), StatusCode::OK);
    assert!(elapsed < Duration::from_millis(200));
}
