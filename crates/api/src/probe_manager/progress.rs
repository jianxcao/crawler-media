use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

use super::ProbeManager;
use crate::fingerprint_job::CaptureGate;

#[derive(Clone)]
pub struct MetadataPriorityGate {
    metadata_pending: Arc<AtomicUsize>,
    metadata_idle: Arc<Notify>,
    shutting_down: Arc<std::sync::atomic::AtomicBool>,
    store: Arc<crate::Store>,
    registry: Option<Arc<crate::library_lifecycle::LibraryWorkRegistry>>,
    kind: crate::library_lifecycle::LibraryWorkKind,
    owners: Option<Arc<std::collections::HashMap<String, (String, marker::WorkControl)>>>,
}

impl MetadataPriorityGate {
    pub fn new(manager: Arc<ProbeManager>) -> Self {
        Self {
            metadata_pending: manager.metadata_pending.clone(),
            metadata_idle: manager.metadata_idle.clone(),
            shutting_down: manager.shutting_down.clone(),
            store: manager.store.clone(),
            registry: manager.library_work.lock().clone(),
            kind: crate::library_lifecycle::LibraryWorkKind::AutomaticDetection,
            owners: None,
        }
    }

    pub fn from_manager(manager: &ProbeManager) -> Self {
        Self {
            metadata_pending: manager.metadata_pending.clone(),
            metadata_idle: manager.metadata_idle.clone(),
            shutting_down: manager.shutting_down.clone(),
            store: manager.store.clone(),
            registry: manager.library_work.lock().clone(),
            kind: crate::library_lifecycle::LibraryWorkKind::AutomaticDetection,
            owners: None,
        }
    }

    /// Retain one lease per original strict owner for the entire expanded pipeline.
    pub(super) fn for_rows(
        manager: &ProbeManager,
        units: &[super::ProbeUnit],
    ) -> Result<(Self, Vec<crate::library_lifecycle::LibraryWorkLease>), String> {
        Self::from_manager(manager).lease_rows(units)
    }

    pub(super) fn lease_rows(
        mut self,
        units: &[super::ProbeUnit],
    ) -> Result<(Self, Vec<crate::library_lifecycle::LibraryWorkLease>), String> {
        let gate = &mut self;
        if let Some(unit) = units.first() {
            gate.kind = super::tracks::optional_work_kind(unit);
        }
        let Some(registry) = gate.registry.as_ref() else {
            return Ok((self, Vec::new()));
        };
        let owners = {
            let store = gate.store.as_ref();
            units
                .iter()
                .map(|unit| {
                    crate::fingerprint_job::source::ensure_source_recorded(&store, &unit.row)?;
                    Ok((
                        unit.row.file.file_id.to_string(),
                        unit.row.file.library_id.to_string(),
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?
        };
        let mut leases = Vec::new();
        let mut controls = std::collections::HashMap::new();
        for (_, owner) in &owners {
            if !controls.contains_key(owner) {
                let lease = registry
                    .acquire_optional(&gate.store, owner, gate.kind)
                    .map_err(|error| error.to_string())?;
                controls.insert(owner.clone(), lease.control().signal().clone());
                leases.push(lease);
            }
        }
        gate.owners = Some(Arc::new(
            owners
                .into_iter()
                .map(|(row, owner)| {
                    let control = controls[&owner].clone();
                    (row, (owner, control))
                })
                .collect(),
        ));
        for unit in units {
            if gate.work_control(&unit.row).is_cancelled() {
                return Err("Library probe source cancelled or changed".into());
            }
        }
        Ok((self, leases))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn expanded_owners_remain_leased_and_cancel_independently() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(store::Store::open(temp.path()).unwrap());
        let manager = ProbeManager::new(store.clone()).unwrap();
        let registry = Arc::new(crate::library_lifecycle::LibraryWorkRegistry::default());
        *manager.library_work.lock() = Some(registry.clone());
        let media = domain::Media {
            id: domain::MediaId::new(),
            kind: domain::MediaKind::Tv,
            title: "Fixture".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.as_ref().insert_media(&media).unwrap();
        let mut units = Vec::new();
        let mut libraries = Vec::new();
        for name in ["a", "b"] {
            let root = temp.path().join(name);
            let owner = store
                .as_ref()
                .create_library(
                    media.kind,
                    name,
                    &[root.to_str().unwrap()],
                    "everyone",
                    true,
                    &[],
                )
                .unwrap();
            store
                .as_ref()
                .set_library_intro_settings(&owner.id, true, true)
                .unwrap();
            let mut row = super::super::test_support::coverage(
                media.id,
                &root.join("episode.mkv").to_string_lossy(),
                Some(1),
                Some(1),
            );
            super::super::test_support::publish_into(&store.as_ref(), &mut row);
            units.push(super::super::ProbeUnit {
                row,
                kind: media.kind,
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: true,
                reuse_media_info_cache: true,
                marker_refresh_id: None,
                job_id: None,
            });
            libraries.push(owner);
        }
        let unleased = MetadataPriorityGate::from_manager(&manager);
        assert!(unleased.work_control(&units[0].row).is_cancelled());
        let (gate, leases) = MetadataPriorityGate::for_rows(&manager, &units).unwrap();
        assert_eq!(leases.len(), 2);
        assert!(!gate.work_control(&units[1].row).is_cancelled());
        registry.cancel(&libraries[1].id);
        assert!(gate.work_control(&units[1].row).is_cancelled());
        assert!(!gate.work_control(&units[0].row).is_cancelled());
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(10),
                registry.wait_idle(&libraries[1].id)
            )
            .await
            .is_err()
        );
        drop(leases);
        registry.wait_idle(&libraries[1].id).await;
    }
}

impl CaptureGate for MetadataPriorityGate {
    fn work_control(&self, row: &domain::FileCoverage) -> marker::WorkControl {
        if self.registry.is_none() {
            return marker::WorkControl::default();
        }
        let owner = crate::fingerprint_job::source::ensure_source_recorded(&self.store, row)
            .ok()
            .map(|()| row.file.library_id.to_string());
        // A registry-backed capture without an active original lease fails closed.
        // Never switch an old pipeline to a new Library at the same path.
        if let Some(owners) = &self.owners {
            if let Some((original, control)) = owners.get(&row.file.file_id.to_string())
                && owner.as_ref().is_some_and(|owner| owner == original)
            {
                return control.clone();
            }
        } else if let Some(control) = owner.and_then(|owner| {
            self.registry
                .as_ref()
                .and_then(|registry| registry.active_control(&owner, self.kind))
        }) {
            return control.signal().clone();
        }
        let cancelled = marker::WorkControl::default();
        cancelled.cancel();
        cancelled
    }
    fn wait_before_capture(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            loop {
                let notified = self.metadata_idle.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.metadata_pending.load(Ordering::Acquire) == 0
                    || self.shutting_down.load(Ordering::Acquire)
                {
                    return;
                }
                notified.await;
            }
        })
    }
}
