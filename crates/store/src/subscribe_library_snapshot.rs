use std::collections::{HashMap, HashSet};

use domain::{FileCoverage, LedgerId, MediaKind, Release, Subscribe};
use subscribe::SubscribeFacts;

use crate::library_ownership::LibraryOwnership;
use crate::read_freshness::ReadFingerprint;
use crate::{Store, StoreError};

/// Owned database snapshot for out-of-lock Subscribe library evaluation.
#[derive(Clone, Debug)]
pub struct SubscribeLibrarySnapshot {
    pub subscribe: Subscribe,
    pub kind: MediaKind,
    pub ownership: LibraryOwnership,
    pub saved: SubscribeFacts,
    pub rows: Vec<FileCoverage>,
    pub qualities: HashMap<LedgerId, Result<Release, String>>,
    /// Scoped fingerprint of the target Library's configuration and the
    /// observed media's file facts, re-read before a side effect is admitted.
    pub fingerprint: ReadFingerprint,
}

impl Store {
    /// Revalidate an observation before admitting work. Files created by the
    /// current transfer may be ignored while its other inputs remain unchanged.
    pub fn subscribe_snapshot_is_current(
        &self,
        observed: &SubscribeLibrarySnapshot,
        ignored_files: &HashSet<String>,
    ) -> Result<bool, StoreError> {
        let mut current = self.subscribe_library_snapshot(&observed.subscribe, observed.kind)?;
        let mut expected = observed.clone();
        for snapshot in [&mut current, &mut expected] {
            snapshot
                .rows
                .retain(|row| !ignored_files.contains(&row.file.file_id.to_string()));
            snapshot
                .qualities
                .retain(|id, _| !ignored_files.contains(&id.to_string()));
            snapshot
                .fingerprint
                .files
                .retain(|file| !ignored_files.contains(&file.file_id));
        }
        let same_ownership = expected.ownership.libraries() == current.ownership.libraries()
            && expected.ownership.libraries().iter().all(|library| {
                expected.ownership.accepts_work(&library.id)
                    == current.ownership.accepts_work(&library.id)
            });
        Ok(same_ownership
            && expected.fingerprint == current.fingerprint
            && expected.saved == current.saved
            && expected.rows == current.rows
            && expected.qualities == current.qualities)
    }

    /// Collect the owned database facts needed to resolve Subscribe facts
    /// without holding the Store lock during filesystem resolution.
    pub fn subscribe_library_snapshot(
        &self,
        subscribe: &Subscribe,
        kind: MediaKind,
    ) -> Result<SubscribeLibrarySnapshot, StoreError> {
        let saved = self.load_subscribe_facts(subscribe.id)?;
        let ownership = self.library_ownership()?;
        let target_library_id = match subscribe.library_id {
            Some(id) => id.to_string(),
            None => ownership
                .default_for(kind)
                .map(|library| library.id.clone())
                .ok_or_else(|| StoreError::Missing("target Library".into()))?,
        };
        let fingerprint = self.read_fingerprint(&target_library_id, subscribe.media_id)?;
        let rows = self.file_coverage_for_media(subscribe.media_id)?;
        let mut qualities = HashMap::new();
        for coverage in &rows {
            qualities.insert(
                coverage.file.file_id,
                self.owned_quality(coverage).map_err(|e| e.to_string()),
            );
        }

        Ok(SubscribeLibrarySnapshot {
            subscribe: subscribe.clone(),
            kind,
            ownership,
            saved,
            rows,
            qualities,
            fingerprint,
        })
    }
}
