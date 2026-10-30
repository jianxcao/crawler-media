//! The single production path that adopts provider metadata: the catalog parses its own
//! validated full response into a normalized snapshot, and the Store recomposes the
//! effective row from every current snapshot. No caller rebuilds a lossy payload.
use std::sync::Arc;

use domain::{EntityScope, Media, MetadataAdoption, MetadataPreferences};

use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;

pub struct MetadataService {
    state: Arc<ApiState>,
}

/// Metadata language / certification preferences from the instance scrape config.
pub(crate) fn metadata_preferences(state: &ApiState) -> MetadataPreferences {
    state
        .store
        .as_ref()
        .get_scrape_config()
        .ok()
        .map(|config| MetadataPreferences {
            language_priority: config.effective.language_priority,
            cert_country_priority: config.effective.cert_country_priority,
        })
        .unwrap_or_else(|| MetadataPreferences {
            language_priority: crate::scrape_config::default_language_priority(),
            cert_country_priority: crate::scrape_config::default_cert_country_priority(),
        })
}

/// The Media metadata revision a provider fetch is bound to, read under the Store lock
/// before the network call.
pub(crate) fn capture_metadata_revision(
    state: &ApiState,
    media: domain::MediaId,
) -> Result<i64, String> {
    state
        .store
        .as_ref()
        .media_revision(media)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("media {media} not found"))
}

/// Fetch (outside the Store lock) and adopt the TMDB snapshot for one entity.
/// Returns `Ok(false)` when the catalog has no normalized response for it.
pub(crate) fn adopt_catalog_metadata(
    state: &ApiState,
    media: &Media,
    tmdb_id: &str,
    scope: EntityScope,
) -> Result<bool, String> {
    let preferences = metadata_preferences(state);
    let locale = crate::scrape_metadata::preferred_language(state);
    // Capture the identity before the network call; the Store CAS refuses a late response.
    let expected_revision = capture_metadata_revision(state, media.id)?;
    let Some(normalized) =
        state
            .catalog
            .normalized_metadata(media.kind, tmdb_id, scope, &preferences)?
    else {
        return Ok(false);
    };
    let fetched_at = crate::job_loop::unix_now() * 1000;
    let adoption = MetadataAdoption {
        media_id: media.id,
        scope,
        locale,
        source: "tmdb".into(),
        provider_entity_id: tmdb_id.to_string(),
        provider_work_id: tmdb_id.to_string(),
        fetched_at,
        normalized,
        expected_revision: Some(expected_revision),
        preferences,
    };
    state
        .store
        .as_ref()
        .adopt_metadata(&adoption, fetched_at)
        .map_err(|e| e.to_string())?;
    Ok(true)
}

impl MetadataService {
    pub fn new(state: Arc<ApiState>) -> Self {
        Self { state }
    }

    /// Refresh the work-level snapshot from the media's TMDB refresh target.
    pub fn refresh_media_metadata(&self, media: &Media) -> Result<(), String> {
        let target = self
            .state
            .store
            .as_ref()
            .media_refresh_alias(media.id, "tmdb")
            .map_err(|e| e.to_string())?;
        let Some(tmdb_id) = target
            .map(|alias| alias.external_id)
            .or_else(|| media.tmdb_id.clone())
        else {
            return Ok(());
        };
        adopt_catalog_metadata(&self.state, media, &tmdb_id, EntityScope::Work).map(|_| ())
    }
}
