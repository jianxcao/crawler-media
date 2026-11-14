//! Instance startup after owned services have been constructed.

use super::{ApiState, ApiStateError};

impl ApiState {
    pub(super) fn finish_initialization(
        self,
        browser_error: Option<String>,
    ) -> Result<Self, ApiStateError> {
        if let Some(error) = browser_error {
            self.obscura.set_last_error(Some(error));
        }
        self.start_runtime()?;
        // Replay an interrupted cover removal after database startup.
        crate::library_cover_service::purge_removed_library_covers(&self);
        Ok(self)
    }

    fn start_runtime(&self) -> Result<(), ApiStateError> {
        if tokio::runtime::Handle::try_current().is_err() {
            return Ok(());
        }
        {
            crate::http::browser_settings::spawn_reconcile(self);
            crate::library_lifecycle::resume_library_removals(self)
                .map_err(std::io::Error::other)?;
            self.probe.try_start_workers();
            self.chapter_frames.start();
            crate::scan_probe_dispatch::start_dispatcher(self.clone());
            // T11: Old browse projection periodic refresh is obsolete with library_items single table
            match self.store.as_ref().recover_orphaned_scans() {
                Ok(n) if n > 0 => tracing::info!(count = n, "已恢复未完成的扫描操作"),
                Ok(_) => {}
                Err(e) => tracing::error!(%e, "恢复未完成的扫描操作失败"),
            }
            crate::root_reassign_work::start(self);
            crate::rename_work::resume_startup(self.clone());
        }
        Ok(())
    }
}

/// Persist the protocol identity once, independently of runtime worker setup.
pub(super) fn jellyfin_server_id(store: &crate::Store) -> Result<String, crate::store::StoreError> {
    if let Some(id) = store.get_setting("jellyfin.server_id")? {
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    store.put_setting("jellyfin.server_id", &id)?;
    Ok(id)
}
