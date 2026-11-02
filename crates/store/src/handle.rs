//! Shared Store database handles and per-store diagnostics.
use crate::{Writers, connections, path_key};
use std::path::PathBuf;

pub struct Store {
    pub(crate) data_dir: PathBuf,
    /// Times this store's `list_file_coverage` executed its unfiltered
    /// `FROM library_files f LEFT JOIN library_file_units u` statement.
    /// Per store family (a writer and its `open_reader` readers share it), so
    /// concurrent tests never share the diagnostic.
    pub(crate) root_ledger_scans: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Calls to `file_coverage_for_media` on this store. Selection can load the
    /// same media twice; the logical candidate count does not show that read.
    pub(crate) media_ledger_reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Per-store role lookup diagnostic for bounded roster work.
    pub(crate) role_lookups: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub(crate) app_path: PathBuf,
    pub(crate) writers: std::sync::Arc<Writers>,
    pub(crate) readers: connections::Readers,
    pub(crate) aliases: path_key::RootAliases,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("data_dir", &self.data_dir)
            .field("app_path", &self.app_path)
            .finish()
    }
}

impl Store {
    /// Times `list_file_coverage` executed its unfiltered
    /// `FROM library_files f LEFT JOIN library_file_units u` statement.
    ///
    /// Incremented beside that statement, so filtering the returned rows cannot
    /// hide the scan. `file_coverage_for_media` does not increment it.
    pub fn root_ledger_scans(&self) -> usize {
        self.root_ledger_scans
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Times `file_coverage_for_media` executed its indexed read.
    pub fn media_ledger_reads(&self) -> usize {
        self.media_ledger_reads
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn role_lookups(&self) -> usize {
        self.role_lookups.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn reset_role_lookups(&self) {
        self.role_lookups
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn cache_db_path(&self) -> PathBuf {
        self.data_dir.join("cache.db")
    }

    /// Freeze committed root aliases; the returned observer performs IO without Store.
    pub fn nfo_input_observer(&self) -> crate::metadata::nfo_input::NfoInputObserver {
        crate::metadata::nfo_input::NfoInputObserver {
            aliases: std::sync::Arc::new(std::sync::RwLock::new(
                self.aliases
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone(),
            )),
        }
    }
}
