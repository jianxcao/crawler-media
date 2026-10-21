use std::sync::Arc;
use domain::{EntityScope, Media, MetadataAdoption, MetadataPreferences};
use crate::management::ApiState;

pub struct MetadataService {
    state: Arc<ApiState>,
}

impl MetadataService {
    pub fn new(state: Arc<ApiState>) -> Self {
        Self { state }
    }

    pub fn refresh_media_metadata(
        &self,
        media: &Media,
        locale: &str,
    ) -> Result<(), String> {
        let store = self.state.store.lock();
        let target = store
            .media_refresh_alias(media.id, "tmdb")
            .map_err(|e| e.to_string())?;
        let Some(refresh_target) = target.or_else(|| {
            media.tmdb_id.clone().map(|ext_id| domain::MediaAlias {
                source: "tmdb".into(),
                kind: media.kind,
                external_id: ext_id,
            })
        }) else {
            return Ok(());
        };
        let tmdb_id = refresh_target.external_id;
        drop(store);

        let preferences = MetadataPreferences {
            language_priority: vec![locale.to_string()],
            cert_country_priority: vec!["CN".into(), "US".into()],
        };

        let normalized = self
            .state
            .catalog
            .normalized_metadata(media.kind, &tmdb_id, EntityScope::Work, &preferences)?;

        if let Some(norm) = normalized {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            let adoption = MetadataAdoption {
                media_id: media.id,
                scope: EntityScope::Work,
                locale: locale.to_string(),
                source: "tmdb".into(),
                provider_entity_id: tmdb_id.clone(),
                provider_work_id: tmdb_id,
                fetched_at: now,
                normalized: norm,
                expected_revision: None,
                preferences,
            };
            self.state
                .store
                .lock()
                .adopt_metadata(&adoption, now)
                .map_err(|e| e.to_string())?;
        }

        Ok(())
    }
}
