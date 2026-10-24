//! Store extensions for the scrape & organize configuration.
//! Separated from `crates/store` so that store does not depend on scrape composition rules.

use crate::scrape_config::{KEY, ScrapeConfig, ScrapeConfigSetting, compose_config};
use crate::store::{Store, StoreError};

pub trait ScrapeStoreExt {
    fn get_scrape_config(&self) -> Result<ScrapeConfig, StoreError>;
    fn save_scrape_config(&self, setting: &ScrapeConfigSetting)
    -> Result<ScrapeConfig, StoreError>;
    fn naming_pattern(&self, kind: domain::MediaKind) -> Result<String, StoreError>;
}

impl ScrapeStoreExt for Store {
    fn get_scrape_config(&self) -> Result<ScrapeConfig, StoreError> {
        let setting = match self.get_setting(KEY)? {
            None => ScrapeConfigSetting::default(),
            Some(raw) => match serde_json::from_str::<ScrapeConfigSetting>(&raw) {
                Ok(setting) => setting,
                Err(error) => {
                    // Fall back to defaults, but never rewrite the stored value:
                    // the corrupt raw text is the only copy an operator can
                    // repair, and silently persisting defaults would destroy it.
                    tracing::error!(
                        %error,
                        key = KEY,
                        "scrape config JSON is corrupt; falling back to defaults and keeping the raw value"
                    );
                    ScrapeConfigSetting::default()
                }
            },
        };
        Ok(compose_config(setting))
    }

    fn save_scrape_config(
        &self,
        setting: &ScrapeConfigSetting,
    ) -> Result<ScrapeConfig, StoreError> {
        self.put_setting(KEY, &serde_json::to_string(setting)?)?;
        Ok(compose_config(setting.clone()))
    }

    /// Composed full naming pattern for a media kind from the effective config.
    fn naming_pattern(&self, kind: domain::MediaKind) -> Result<String, StoreError> {
        let effective = self.get_scrape_config()?.effective;
        Ok(match kind {
            domain::MediaKind::Movie | domain::MediaKind::Video => {
                effective.compose_movie_pattern()
            }
            domain::MediaKind::Tv => effective.compose_tv_pattern(),
        })
    }
}
