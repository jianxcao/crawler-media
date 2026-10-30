use api::catalog::Catalog;
use api::password_login::PasswordEngine;
use domain::{Media, MediaKind};
use indexer::{FetchRequest, Fetcher, IndexerError};
use media::CatalogHit;

pub const PASSWORD: &str = "interactive-fixture-password";

/// Keep password CPU scheduling deterministic; the Store still owns the real
/// Argon2 credential row, final CAS, transaction and token issuance.
pub struct ImmediatePassword;

impl PasswordEngine for ImmediatePassword {
    fn verify(&self, password: &str, stored: &str) -> bool {
        password == PASSWORD && store::password::is_argon2_hash(stored)
    }
}

pub struct OfflineFetcher;

impl Fetcher for OfflineFetcher {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline interactive fixture".into()))
    }
}

/// Optional catalog enrichment finishes locally. This test isolates writer
/// contention from the separate catalog latency fixes in Task12.1.
pub struct OfflineCatalog;

impl Catalog for OfflineCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }

    fn search_tv(&self, _: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }

    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }

    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }

    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
}
