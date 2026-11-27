use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use super::ApiState;

pub(crate) struct InFlightDownloadOpGuard {
    ops: Arc<Mutex<HashMap<String, usize>>>,
    op_id: String,
}

impl Drop for InFlightDownloadOpGuard {
    fn drop(&mut self) {
        let mut ops = self.ops.lock();
        if let Some(count) = ops.get_mut(&self.op_id) {
            if *count <= 1 {
                ops.remove(&self.op_id);
            } else {
                *count -= 1;
            }
        }
    }
}

impl ApiState {
    pub(crate) fn enter_download_op_in_flight(&self, op_id: String) -> InFlightDownloadOpGuard {
        {
            let mut ops = self.in_flight_download_ops.lock();
            *ops.entry(op_id.clone()).or_insert(0) += 1;
        }
        InFlightDownloadOpGuard {
            ops: Arc::clone(&self.in_flight_download_ops),
            op_id,
        }
    }

    pub(crate) fn is_download_op_in_flight(&self, op_id: &str) -> bool {
        self.in_flight_download_ops.lock().contains_key(op_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use indexer::{Fetcher, RegistrySnapshot};

    struct StubFetcher;
    impl Fetcher for StubFetcher {
        fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
            Err(indexer::IndexerError::Fetch("unused".into()))
        }
    }

    #[test]
    fn in_flight_download_op_guard_refcounts_concurrent_entries() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let dl = Arc::new(downloader::MemoryDownloader::new(dir.path().join("stage")));
        let state = ApiState::new(
            store,
            "tok".into(),
            RegistrySnapshot::builtin().unwrap(),
            Arc::new(StubFetcher),
            dl,
        )
        .unwrap();

        let g1 = state.enter_download_op_in_flight("op-race".into());
        assert!(state.is_download_op_in_flight("op-race"));
        let g2 = state.enter_download_op_in_flight("op-race".into());
        assert!(state.is_download_op_in_flight("op-race"));
        drop(g2);
        assert!(
            state.is_download_op_in_flight("op-race"),
            "winner must stay in-flight when loser drops guard"
        );
        drop(g1);
        assert!(!state.is_download_op_in_flight("op-race"));
    }
}
