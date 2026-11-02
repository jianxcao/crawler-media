//! Release STRM observations when their last successfully watched root retires.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use notify_debouncer_mini::DebouncedEvent;

use super::{FsWatcherState, StrmGraceTracker};

impl FsWatcherState {
    pub(crate) fn relevant_events(&self, events: &[DebouncedEvent]) -> Vec<DebouncedEvent> {
        super::relevant_fs_events(events)
            .into_iter()
            .filter(|event| self.active.iter().any(|root| event.path.starts_with(root)))
            .collect()
    }
}

impl StrmGraceTracker {
    pub(super) fn retain_roots(&self, roots: &HashSet<PathBuf>) {
        let covered = |path: &Path| roots.iter().any(|root| path.starts_with(root));
        let mut known_urls = self.known_urls.lock();
        known_urls.retain(|path, _| covered(path));
        release_unused(&mut known_urls);
        drop(known_urls);
        let mut pending = self.pending.lock();
        pending.retain(|path, _| covered(path));
        release_unused(&mut pending);
    }
}

fn release_unused<V>(map: &mut HashMap<PathBuf, V>) {
    if map.is_empty() {
        *map = HashMap::new();
    } else if map.capacity() > map.len().saturating_mul(4) {
        map.shrink_to_fit();
    }
}
