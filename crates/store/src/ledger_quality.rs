use domain::{Confidence, FileCoverage, Release};
use rusqlite::{OptionalExtension, params};
use subscribe::SubscribeFacts;

use super::{Store, StoreError};

/// The coordinates a per-unit row carries; a whole-file coverage has none.
///
/// A coverage may hold several units（同一物理文件覆盖多集）。这里取升序首个 unit
/// 作为 Release 的坐标。
fn coverage_coordinates(coverage: &FileCoverage) -> (Option<u32>, Option<u32>) {
    match coverage.units.first() {
        Some(unit) if !unit.is_whole() => (
            u32::try_from(unit.season).ok(),
            u32::try_from(unit.episode).ok(),
        ),
        _ => (None, None),
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredQuality {
    resolution: Option<String>,
    source: Option<String>,
    codec: Option<String>,
    hdr: Option<String>,
}

impl From<&Release> for StoredQuality {
    fn from(release: &Release) -> Self {
        Self {
            resolution: release.resolution.clone(),
            source: release.source.clone(),
            codec: release.codec.clone(),
            hdr: release.hdr.clone(),
        }
    }
}

impl Store {
    /// Persist provenance dimensions that stream probing cannot recover.
    /// The Library row owns this quality, so a later Subscribe can reuse it.
    pub(crate) fn persist_owned_quality(&self, facts: &SubscribeFacts) -> Result<(), StoreError> {
        self.with_library_write(|db| db.persist_owned_quality(facts))
    }

    /// Load stable Release facts and overlay the latest stream probe fields.
    pub(crate) fn owned_quality(&self, coverage: &FileCoverage) -> Result<Release, StoreError> {
        self.with_library_read(|db| db.owned_quality(coverage))
    }
}

#[path = "ledger_quality/connection.rs"]
mod connection;
