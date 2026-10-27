//! The scan-operation test fixture: a Store with one seeded Library plus the helper that
//! drives an operation to its terminal state.
//!
//! The re-exports below are the imports the scenario modules share; each of them does
//! `use super::fixture::*;` instead of repeating this list.

use std::path::PathBuf;

use crate::scan_operations::scan_scope_signature;

pub(crate) use domain::{Confidence, FileCoverage, Media, MediaId, MediaKind, QualitySource};
pub(crate) use std::sync::Arc;
pub(crate) use store::{ScanOperation, ScanOperationStatus};

pub(crate) use crate::management::ApiState;



pub(crate) struct NoFetch;

impl indexer::Fetcher for NoFetch {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch("offline".into()))
    }
}



pub(crate) struct Fixture {
    pub(crate) _dir: tempfile::TempDir,
    pub(crate) state: ApiState,
    pub(crate) library_id: String,
    pub(crate) root: PathBuf,
}



impl Fixture {
    pub(crate) fn new() -> Self {
        Self::with_kind(MediaKind::Movie)
    }

    pub(crate) fn with_kind(kind: MediaKind) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::Store::open(dir.path().join("data")).unwrap();
        let state = ApiState::new(
            store,
            "test-token".into(),
            indexer::ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(downloader::MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap();
        let root = dir.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        let library_id = {
            let store = state.store();
            let guard = store.lock();
            guard
                .create_library(
                    kind,
                    "Movies",
                    &[root.to_str().unwrap()],
                    "everyone",
                    true,
                    &[],
                )
                .unwrap()
                .id
        };
        Self {
            _dir: dir,
            state,
            library_id,
            root,
        }
    }

    pub(crate) fn signature(&self) -> String {
        let store = self.state.store();
        let guard = store.lock();
        let library = guard.get_library(&self.library_id).unwrap().unwrap();
        scan_scope_signature(&library)
    }

    pub(crate) fn insert_media_and_ledger(&self, rel_path: &str) -> (MediaId, FileCoverage) {
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "Test Movie".to_string(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        let path = self.root.join(rel_path).to_str().unwrap().to_string();
        let spec = store::test_support::FixtureFile {
            media_id: media.id,
            path: path.clone(),
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            ..Default::default()
        };
        let store = self.state.store();
        let guard = store.lock();
        guard.insert_media(&media).unwrap();
        store::test_support::publish_fixture_file(&guard, &spec).unwrap();
        let coverage = guard.file_coverage_by_path(&path).unwrap().unwrap();
        (media.id, coverage)
    }

    pub(crate) async fn wait_terminal(&self, op_id: &str) -> ScanOperation {
        for _ in 0..600 {
            let op = self.state.store().lock().scan_operation(op_id).unwrap();
            if let Some(op) = op
                && !op.status.is_active()
            {
                return op;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("scan operation {op_id} 未在预期时间内进入终态");
    }
}
