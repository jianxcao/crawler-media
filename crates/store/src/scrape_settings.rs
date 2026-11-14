//! Raw Scrape authority; composition belongs to API. Revision prevents ABA plans.
use crate::{Store, StoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawScrapeSettings {
    pub revision: i64,
    pub value: Option<String>,
}

impl Store {
    pub fn raw_scrape_settings(&self) -> Result<RawScrapeSettings, StoreError> {
        self.with_library_read(|db| db.raw_scrape_settings())
    }

    pub fn save_raw_scrape_settings(&self, raw: &str) -> Result<(), StoreError> {
        self.library_write(|db| db.save_raw_scrape_settings(raw))
    }
}

impl crate::LibraryConnection<'_> {
    pub fn raw_scrape_settings(&self) -> Result<RawScrapeSettings, StoreError> {
        self.library
            .query_row(
                "SELECT revision, value FROM scrape_settings WHERE singleton=1",
                [],
                |row| {
                    Ok(RawScrapeSettings {
                        revision: row.get(0)?,
                        value: row.get(1)?,
                    })
                },
            )
            .map_err(Into::into)
    }

    pub fn save_raw_scrape_settings(&self, raw: &str) -> Result<(), StoreError> {
        let revision = self
            .raw_scrape_settings()?
            .revision
            .checked_add(1)
            .ok_or_else(|| StoreError::Invalid("Scrape revision overflow".into()))?;
        self.library.execute(
            "UPDATE scrape_settings SET revision=?1, value=?2 WHERE singleton=1",
            rusqlite::params![revision, raw],
        )?;
        tracing::info!(revision, "Scrape settings updated");
        Ok(())
    }
}
