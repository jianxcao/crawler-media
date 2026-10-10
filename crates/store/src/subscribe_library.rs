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
        self.library_subscribe_facts(subscribe, kind)
            .map_err(|error| {
                tracing::error!(subscribe_id = %subscribe.id, library_id = ?subscribe.library_id,
                %error, "加载目标 Library 订阅事实失败");
                error
            })
    }

    fn library_subscribe_facts(
        &self,
        subscribe: &Subscribe,
        kind: MediaKind,
    ) -> Result<SubscribeFacts, StoreError> {
        let saved = self.load_subscribe_facts(subscribe.id)?;
        let mut facts = SubscribeFacts::default();
        let library = match subscribe.library_id {
            Some(id) => self.get_library(&id.to_string())?,
            None => self.default_library(kind)?,
        }
        .ok_or_else(|| StoreError::Missing("target Library".into()))?;
        if library.kind != kind {
            return Err(StoreError::Protected(
                "target Library kind does not match Media".into(),
            ));
        }
        for ((season, episode), fact) in saved.entries() {
            let Some(path) = fact.path.as_deref() else {
                continue;
            };
            // Missing files still carry deletion facts, but only for their unique
            // strict owner. Neither the default Library nor ambiguity grants ownership.
            if self
                .library_for_path_strict(Path::new(path), kind)?
                .is_none_or(|owner| owner.id != library.id)
            {
                tracing::info!(subscribe_id = %subscribe.id, library_id = %library.id,
                    season, episode, path,
                    "已保存事实不属于目标 Library，本次不把它当作已拥有内容");
                continue;
            }
            facts.replace(season, episode, fact.clone());
            if let Some(quality) = saved.quality(path) {
                facts.set_quality(path.to_string(), quality.clone());
            }
        }
        for row in self.ledger_for_media(subscribe.media_id)? {
            let path = Path::new(&row.path);
            if !path.is_file() {
                continue;
            }
            if self
                .library_for_path_strict(path, kind)?
                .is_none_or(|owner| owner.id != library.id)
            {
                continue;
            }
            let fact = QualityFact {
                score: row.filter_score.unwrap_or(0),
                path: Some(row.path.clone()),
            };
            let key = (row.season, row.episode);
            let stale = facts
                .get(key.0, key.1)
                .and_then(|f| f.path.as_deref())
                .map(|old| self.ledger_by_path(old).map(|row| row.is_none()))
                .transpose()?
                .unwrap_or(false);
            if stale {
                facts.replace(key.0, key.1, fact);
            } else {
                facts.upsert(key.0, key.1, fact);
            }
            facts.set_quality(row.path.clone(), self.owned_quality(&row)?);
        }
        // Ledger merging can replace a saved path or reject a lower-scored row.
        // Keep quality only for paths that actually survived in the owned slots.
        let mut scoped = SubscribeFacts::default();
        for ((season, episode), fact) in facts.entries() {
            scoped.replace(season, episode, fact.clone());
            if let Some(path) = fact.path.as_deref() {
                if let Some(quality) = facts.quality(path) {
                    scoped.set_quality(path.to_string(), quality.clone());
                }
            }
        }
        Ok(scoped)
    }
}
