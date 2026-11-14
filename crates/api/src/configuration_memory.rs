//! Process-local parsed TMDB configuration tables.
//!
//! A hit reads only this memory. A miss is filled by the existing catalog
//! fetch, which stays on the caller's blocking budget.

use std::sync::Arc;

use media::{CountryRow, LanguageRow};
use parking_lot::RwLock;

#[derive(Default)]
pub(crate) struct ConfigurationMemory {
    languages: RwLock<Option<Arc<Vec<LanguageRow>>>>,
    countries: RwLock<Option<Arc<Vec<CountryRow>>>>,
}

impl ConfigurationMemory {
    pub(crate) fn languages(&self) -> Option<Arc<Vec<LanguageRow>>> {
        self.languages.read().clone()
    }

    pub(crate) fn countries(&self) -> Option<Arc<Vec<CountryRow>>> {
        self.countries.read().clone()
    }

    pub(crate) fn store_languages(&self, rows: Vec<LanguageRow>) {
        *self.languages.write() = Some(Arc::new(rows));
    }

    pub(crate) fn store_countries(&self, rows: Vec<CountryRow>) {
        *self.countries.write() = Some(Arc::new(rows));
    }
}
