use std::collections::HashMap;

use domain::{LedgerId, LedgerRow, MediaKind, Release, Subscribe};
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
    pub rows: Vec<LedgerRow>,
    pub qualities: HashMap<LedgerId, Result<Release, String>>,
    /// Scoped fingerprint of the target Library's configuration and the
    /// observed media's file facts, re-read before a side effect is admitted.
    pub fingerprint: ReadFingerprint,
}

impl Store {
    /// Collect the owned database facts needed to resolve Subscribe facts
    /// without holding the Store lock during filesystem resolution.
    pub fn subscribe_library_snapshot(
        &self,
        subscribe: &Subscribe,
        kind: MediaKind,
    ) -> Result<SubscribeLibrarySnapshot, StoreError> {
        let saved = self.load_subscribe_facts(subscribe.id)?;
        let ownership = self.library_ownership()?;
        let target_library_id = match subscribe.library_id.clone() {
            Some(id) => id.to_string(),
            None => ownership
                .default_for(kind)
                .map(|library| library.id.clone())
                .ok_or_else(|| StoreError::Missing("target Library".into()))?,
        };
        let fingerprint = self.read_fingerprint(&target_library_id, subscribe.media_id)?;
        let rows = self.ledger_for_media(subscribe.media_id)?;
        let mut qualities = HashMap::new();
        for row in &rows {
            qualities.insert(row.id, self.owned_quality(row).map_err(|e| e.to_string()));
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
