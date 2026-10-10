use std::path::Path;

use domain::{MediaKind, Subscribe};
use subscribe::{QualityFact, SubscribeFacts};

use super::{Store, StoreError};

impl Store {
    /// Coordinate a Subscribe's local facts with the selected shared Library.
    /// In-flight coverage is deliberately absent; recorded user deletion facts
    /// are retained instead of implicitly authorizing another download.
    pub fn load_library_subscribe_facts(
        &self,
        subscribe: &Subscribe,
        kind: MediaKind,
    ) -> Result<SubscribeFacts, StoreError> {
        let mut facts = self.load_subscribe_facts(subscribe.id)?;
        facts.retain_owned();
        let library = match subscribe.library_id {
            Some(id) => self.get_library(&id.to_string())?,
            None => self.default_library(kind)?,
        }.ok_or_else(|| StoreError::Missing("target Library".into()))?;
        if library.kind != kind {
            return Err(StoreError::Protected("target Library kind does not match Media".into()));
        }
        for row in self.ledger_for_media(subscribe.media_id)? {
            let path = Path::new(&row.path);
            if !library.root_paths.iter().any(|root| path.starts_with(root)) || !path.is_file() {
                continue;
            }
            if self.library_for_path(path, kind)?.is_none_or(|owner| owner.id != library.id) {
                continue;
            }
            let fact = QualityFact { score: row.filter_score.unwrap_or(0), path: Some(row.path.clone()) };
            let key = (row.season, row.episode);
            let stale = facts.get(key.0, key.1).and_then(|f| f.path.as_deref())
                .map(|old| self.ledger_by_path(old).map(|row| row.is_none()))
                .transpose()?.unwrap_or(false);
            if stale {
                facts.replace(key.0, key.1, fact);
            } else {
                facts.upsert(key.0, key.1, fact);
            }
            facts.set_quality(row.path.clone(), self.owned_quality(&row)?);
        }
        Ok(facts)
    }
}
