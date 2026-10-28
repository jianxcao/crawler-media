//! Transfer deletion observations. Filesystem calls run outside the Store guard.
use crate::management::ApiState;
use std::path::Path;

pub(crate) fn observe_removal_identity(
    state: &ApiState,
    path: &str,
) -> Result<Option<(u64, Option<u64>)>, String> {
    let metadata = match state.file_stats.symlink_metadata(Path::new(path)) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            tracing::error!(%error,path,"Transfer deletion identity observation failed");
            return Err(error.to_string());
        }
    };
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        Some(metadata.ino())
    };
    #[cfg(not(unix))]
    let inode = None;
    Ok(Some((metadata.len(), inode)))
}

/// The identity authorized by the original read or this round's publication.
/// `disk_source` differs from the reserved version while placement is unbound.
#[derive(Clone)]
pub(crate) struct TransferRemovalPin {
    file_id: domain::LedgerId,
    library_id: String,
    media_id: domain::MediaId,
    root_id: String,
    path: std::path::PathBuf,
    version: domain::FileVersion,
    units: Vec<domain::UnitKey>,
    presence: String,
    publication: String,
    disk_source: String,
    placed_source: Option<library::SourceObservation>,
}

impl TransferRemovalPin {
    pub(super) fn published(
        coverage: &domain::FileCoverage,
        source: &library::SourceObservation,
    ) -> Self {
        let file = &coverage.file;
        Self {
            file_id: file.file_id,
            library_id: file.library_id.to_string(),
            media_id: file.media_id,
            root_id: file.root_id.clone(),
            path: file.path.clone(),
            version: file.version.clone(),
            units: coverage.units.clone(),
            presence: file.presence.as_str().into(),
            publication: file.publication.as_str().into(),
            disk_source: source.source_version.clone(),
            placed_source: Some(source.clone()),
        }
    }

    fn original(
        fingerprint: &store::ReadFingerprint,
        file: &store::FileReadRevision,
    ) -> Result<Self, String> {
        Ok(Self {
            file_id: file
                .file_id
                .parse()
                .map_err(|error: uuid::Error| error.to_string())?,
            library_id: fingerprint.library_id.clone(),
            media_id: fingerprint.media_id,
            root_id: file.root_id.clone(),
            path: file.path.clone().into(),
            version: domain::FileVersion {
                revision: file.file_revision,
                source_version: file.source_version.clone(),
            },
            units: file.units.clone(),
            presence: file.presence_state.clone(),
            publication: file.publication_state.clone(),
            disk_source: file.source_version.clone(),
            placed_source: None,
        })
    }

    fn validate(&self, store: &store::Store) -> Result<(), String> {
        let current = store
            .file_coverage_by_id(self.file_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("Transfer deletion file {} disappeared", self.file_id))?;
        let file = &current.file;
        if file.library_id.to_string() != self.library_id
            || file.media_id != self.media_id
            || file.root_id != self.root_id
            || file.path != self.path
            || file.version != self.version
            || current.units != self.units
            || file.presence.as_str() != self.presence
            || file.publication.as_str() != self.publication
        {
            tracing::warn!(file_id=%self.file_id,path=%self.path.display(),"Transfer deletion source or ownership changed");
            return Err(format!(
                "Transfer deletion source or ownership changed: {}",
                self.path.display()
            ));
        }
        let owner = store
            .library_root_owning_key(Path::new(&current.path_key), file.kind)
            .map_err(|error| error.to_string())?;
        if owner != Some((self.library_id.clone(), self.root_id.clone()))
            || !store
                .library_accepts_work(&self.library_id)
                .map_err(|error| error.to_string())?
            || store
                .library_lifecycle_state(&self.library_id)
                .map_err(|error| error.to_string())?
                != "ready"
        {
            return Err("Transfer deletion Library ownership changed".into());
        }
        Ok(())
    }
}

pub(crate) struct ObservedTransferRemoval {
    pin: TransferRemovalPin,
    identity: Option<(u64, Option<u64>)>,
    config_revision: i64,
}

/// Resolve authority from the original fingerprint or this round's captured
/// publication; never authorize deletion using whichever row occupies a path now.
pub(crate) fn observe_transfer_removals(
    state: &ApiState,
    plan: &super::plan::TransferPlan,
    paths: &[String],
) -> Result<Vec<ObservedTransferRemoval>, String> {
    let requested: std::collections::HashSet<&Path> =
        paths.iter().map(|path| Path::new(path)).collect();
    let mut pins = std::collections::HashMap::new();
    for file in &plan.fingerprint.files {
        if requested.contains(Path::new(&file.path)) {
            pins.insert(
                std::path::PathBuf::from(&file.path),
                TransferRemovalPin::original(&plan.fingerprint, file)?,
            );
        }
    }
    for (path, pin) in plan.published_removals.lock().iter() {
        if requested.contains(path.as_path()) {
            pins.insert(path.clone(), pin.clone());
        }
    }
    paths
        .iter()
        .map(|path| {
            let pin = pins.get(Path::new(path)).cloned().ok_or_else(|| {
                format!(
                    "Transfer deletion path was not observed or published by this round: {path}"
                )
            })?;
            match library::observe_source(&pin.path) {
                Ok(source) if source.source_version == pin.disk_source => {}
                Err(library::LibraryError::Io(error))
                    if error.kind() == std::io::ErrorKind::NotFound => {}
                result => {
                    tracing::warn!(
                        path,
                        ?result,
                        "Transfer deletion disk source no longer matches its authorized version"
                    );
                    return Err(format!("Transfer deletion source changed: {path}"));
                }
            }
            let identity = observe_removal_identity(state, path)?;
            Ok(ObservedTransferRemoval {
                pin,
                identity,
                config_revision: plan.config_revision,
            })
        })
        .collect()
}

/// Called while holding the same Store guard as the enqueue. Validate the whole
/// batch before creating any intent; the pending intents then block source writes.
pub(crate) fn queue_transfer_removals(
    state: &ApiState,
    store: &store::Store,
    removals: &[ObservedTransferRemoval],
) -> Result<(), String> {
    state
        .require_writable()
        .map_err(|_| "instance.read_only".to_string())?;
    state
        .require_write_admitted()
        .map_err(|_| "runtime.busy".to_string())?;
    for removal in removals {
        removal.pin.validate(store)?;
    }
    for removal in removals {
        if let Some(source) = &removal.pin.placed_source
            && source.source_version != removal.pin.version.source_version
        {
            // Vetoed arrivals never reach the normal confirmed-file commit.
            // Bind only the reservation this recorder actually published, so
            // the drain compares the placed bytes to their real source token.
            store
                .bind_placed_file_source(
                    &store::library_write::WriteScope {
                        library_id: removal
                            .pin
                            .library_id
                            .parse()
                            .map_err(|error: uuid::Error| error.to_string())?,
                        config_revision: removal.config_revision,
                    },
                    removal.pin.file_id,
                    &removal.pin.version,
                    source,
                    store::unix_now() * 1000,
                )
                .map_err(|error| error.to_string())?;
        }
        store
            .enqueue_file_deletion_observed(
                &removal.pin.path.to_string_lossy(),
                "wash_replaced",
                removal.identity,
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}
