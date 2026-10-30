use std::path::Path;
use std::sync::Arc;

use api::{ApiState, Store};
use domain::FileCoverage;
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

pub async fn resolved_chapters(
    store: Arc<Store>,
    root: &Path,
    original: &FileCoverage,
) -> Vec<marker::ChapterMarker> {
    let (row, media) = {
        let store = store.as_ref();
        let row = store
            .file_coverage_by_id(original.file.file_id)
            .unwrap()
            .unwrap();
        let media = store.get_media(row.file.media_id).unwrap().unwrap();
        store
            .put_cached_chapters(&row.file.file_id.to_string(), &[])
            .unwrap();
        (row, media)
    };
    let state = ApiState::new_arc(
        store,
        "test-token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap();
    api::marker_resolver::resolve_item_chapters(&state, &row, row.units[0], &media, None).await
}
