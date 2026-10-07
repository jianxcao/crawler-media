use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use api::{ApiState, router};
use axum::http::StatusCode;
use domain::{MediaId, SubscribeId, Torrent};
use downloader::{Downloader, DownloaderError};
use parking_lot::Mutex;
use serde_json::json;
use tower::ServiceExt;

use super::super::common::{Fixtures, json_data, request, state, subscribe_payload};

#[derive(Default)]
pub(super) struct CompletedFiles(Mutex<HashMap<String, Vec<PathBuf>>>);

impl Downloader for CompletedFiles {
    fn add(&self, _: &Torrent) -> Result<(), DownloaderError> {
        Ok(())
    }
    fn remove(&self, _: &Torrent, _: bool) -> Result<(), DownloaderError> {
        Ok(())
    }
    fn completed_files(&self, torrent: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        Ok(self
            .0
            .lock()
            .get(&torrent.enclosure)
            .cloned()
            .unwrap_or_default())
    }
}

pub(super) struct TransferFixture {
    pub tmp: tempfile::TempDir,
    pub state: ApiState,
    pub id: SubscribeId,
    pub media_id: MediaId,
    downloader: Arc<CompletedFiles>,
}

impl TransferFixture {
    pub async fn new(tv: bool, naming: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let downloader = Arc::new(CompletedFiles::default());
        let state = state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            downloader.clone(),
        );
        let app = router(state.clone());
        // `/directory` decomposes both patterns into one shared `naming_entry_dir`,
        // so the kind that is not under test must reuse the same entry segment or
        // it would silently overwrite the tested pattern's directory level.
        let entry = naming.split('/').next().unwrap_or("{title}");
        let tv_naming = if tv {
            naming.to_string()
        } else {
            format!("{entry}/{{season}}/{{episode}}{{ext}}")
        };
        let movie_naming = if tv {
            format!("{entry}/{{title}} ({{year}}){{ext}}")
        } else {
            naming.to_string()
        };
        let response = app
            .clone()
            .oneshot(request(
                "PUT",
                "/api/v1/directory",
                Some("management-secret"),
                json!({
                    "movie_root": tmp.path().join("movies"), "tv_root": tmp.path().join("tv"),
                    "transfer_mode": "copy", "movie_naming": movie_naming, "tv_naming": tv_naming,
                    "scrape": false,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = subscribe_payload("search");
        body["filter"] = json!({
            "name": "retry-1080p",
            "atoms": [{ "kind": "resolution", "value": "1080p", "priority": 100 }]
        });
        body["media"] = if tv {
            json!({"kind": "tv", "title": "The Long Watch"})
        } else {
            json!({"kind": "movie", "title": "The Matrix", "year": 1999})
        };
        if tv {
            body["coverage"] =
                json!({"kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2});
        }
        let response = app
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                body,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let created = json_data(response).await;
        Self {
            tmp,
            state,
            downloader,
            id: created["id"].as_str().unwrap().parse().unwrap(),
            media_id: created["media"]["id"].as_str().unwrap().parse().unwrap(),
        }
    }

    pub fn source(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.tmp.path().join("stage").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    pub fn files(&self, torrent: &Torrent, files: &[PathBuf]) {
        self.downloader
            .0
            .lock()
            .insert(torrent.enclosure.clone(), files.to_vec());
    }

    pub fn pending(&self, torrent: &Torrent) {
        self.state
            .store()
            .lock()
            .record_pending_submission(
                self.id,
                100,
                &api::store::PendingDownload {
                    submitted_at: Some(1000),
                    torrent: torrent.clone(),
                    release_override: None,
                    downloader_id: None,
                },
            )
            .unwrap();
    }

    pub async fn transfer(&self) -> Result<(), String> {
        let state = self.state.clone();
        let id = self.id;
        tokio::task::spawn_blocking(move || api::worker::transfer_one(&state, id))
            .await
            .unwrap()
    }

    pub fn destination(&self, relative: &str) -> PathBuf {
        self.tmp.path().join(relative)
    }

    pub fn assert_pending(&self, active: usize, imported: usize) {
        let store = self.state.store();
        let store = store.lock();
        assert_eq!(store.load_pending(self.id).unwrap().len(), active);
        assert_eq!(
            store.load_pending_state(self.id, "imported").unwrap().len(),
            imported
        );
    }

    pub fn assert_sources(&self, expected: &[(&Path, &Path)]) {
        let rows = self.state.store().lock().list_ledger_with_mode().unwrap();
        assert_eq!(rows.len(), expected.len(), "No duplicate video ledger");
        for (source, destination) in expected {
            let row = rows
                .iter()
                .find(|(row, _, _)| Path::new(&row.path) == *destination)
                .unwrap();
            assert_eq!(row.0.media_id, self.media_id);
            assert_eq!(row.2.as_deref(), source.to_str());
        }
    }

    pub fn unblock(&self, blocker: &Path, relative: &str) {
        let expected = self.tmp.path().canonicalize().unwrap().join(relative);
        assert!(expected.is_absolute());
        assert_eq!(
            blocker.canonicalize().unwrap(),
            expected,
            "Exact canonical blocker target"
        );
        assert!(blocker.is_dir());
        std::fs::remove_dir(blocker).unwrap();
    }
}

pub(super) fn torrent(title: &str, key: &str) -> Torrent {
    Torrent {
        id: None,
        site_id: domain::SiteId::new(),
        title: title.into(),
        enclosure: format!("magnet:?xt=urn:btih:{key}"),
        size_bytes: Some(1024),
        seeders: Some(10),
        free: false,
        hr: false,
        imdb_id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    }
}
