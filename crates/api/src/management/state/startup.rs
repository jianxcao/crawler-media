//! Instance startup after owned services have been constructed.

use std::sync::atomic::Ordering;

use super::{ApiState, ApiStateError, apply_admin_bootstrap, seed_jobs};

impl ApiState {
    pub(super) fn finish_initialization(
        self,
        browser_error: Option<String>,
    ) -> Result<Self, ApiStateError> {
        if let Some(error) = browser_error {
            self.obscura.set_last_error(Some(error));
        }
        if self.instance_writable() {
            self.ensure_writable_runtime_started()?;
            crate::library_cover_service::purge_removed_library_covers(&self);
        }
        Ok(self)
    }

    pub fn ensure_writable_runtime_started(&self) -> Result<(), ApiStateError> {
        if !self.writable_startup_done.swap(true, Ordering::AcqRel)
            && let Err(error) = self.seed_writable_startup_state()
        {
            self.writable_startup_done.store(false, Ordering::Release);
            return Err(error);
        }
        self.start_runtime()?;
        Ok(())
    }

    fn seed_writable_startup_state(&self) -> Result<(), ApiStateError> {
        if let Some(action) = self.deferred_bootstrap.lock().take() {
            apply_admin_bootstrap(&self.store.as_ref(), action)?;
        }
        if self.store.get_setting("jellyfin.server_id")?.is_none() {
            self.store
                .put_setting("jellyfin.server_id", &self.jellyfin_server_id)?;
        }
        {
            let jobs = self.jobs.acquire()?;
            seed_jobs(&jobs)?;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            self.probe.recover_deferred();
        }
        crate::library_cover_service::purge_removed_library_covers(self);
        Ok(())
    }

    fn start_runtime(&self) -> Result<(), ApiStateError> {
        if tokio::runtime::Handle::try_current().is_err() {
            return Ok(());
        }
        if self.runtime_workers_started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        crate::http::browser_settings::spawn_reconcile(self);
        if let Err(error) = crate::library_lifecycle::resume_library_removals(self) {
            self.runtime_workers_started.store(false, Ordering::Release);
            return Err(std::io::Error::other(error).into());
        }
        self.probe.try_start_workers();
        self.chapter_frames.start();
        crate::scan_probe_dispatch::start_dispatcher(self.clone());
        match self.store.as_ref().recover_orphaned_scans() {
            Ok(n) if n > 0 => tracing::info!(count = n, "已恢复未完成的扫描操作"),
            Ok(_) => {}
            Err(e) => tracing::error!(%e, "恢复未完成的扫描操作失败"),
        }
        crate::root_reassign_work::start(self);
        crate::rename_work::resume_startup(self.clone());
        Ok(())
    }
}

/// Persist the protocol identity once when writable; standbys keep an in-memory fallback.
pub(super) fn jellyfin_server_id(
    store: &crate::Store,
    writable: bool,
) -> Result<String, crate::store::StoreError> {
    if let Some(id) = store.get_setting("jellyfin.server_id")? {
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    if writable {
        store.put_setting("jellyfin.server_id", &id)?;
    }
    Ok(id)
}
