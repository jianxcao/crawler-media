//! Placement filesystem observations are captured before SQL publication.
use crate::management::ApiState;
use domain::FileCoverage;
use store::StoreError;
use store::library_write::{FilePublication as PublicationRequest, WriteScope};

pub(crate) struct TransferFileObservation {
    pub(crate) mode: String,
    pub(crate) source: Option<library::SourceObservation>,
    pub(crate) artwork: store::files::DiscoveredArtwork,
}

pub(crate) fn observe_transfer_files(
    state: &ApiState,
    files: &[FileCoverage],
    default_mode: &str,
) -> Vec<TransferFileObservation> {
    let modes = observe_transfer_modes(files, default_mode);
    files
        .iter()
        .zip(modes)
        .map(|(coverage, mode)| TransferFileObservation {
            mode,
            source: crate::fingerprint_job::observe_source(&coverage.file.path),
            artwork: store::files::DiscoveredArtwork::discover_with(&coverage.file.path, |path| {
                state.file_stats.is_file(path)
            }),
        })
        .collect()
}

/// The transfer mode a placed file actually ended up with: a file that shares an
/// inode with its download source came in as a hardlink, everything else kept the
/// configured mode. `library_files.transfer_mode` only accepts the three real
/// transfer modes, so the observation never invents a fourth one.
pub(crate) fn observe_transfer_modes(files: &[FileCoverage], default_mode: &str) -> Vec<String> {
    files
        .iter()
        .map(|coverage| {
            let is_hardlink = std::fs::metadata(&coverage.file.path)
                .map(|meta| {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        meta.nlink() > 1
                    }
                    #[cfg(not(unix))]
                    {
                        false
                    }
                })
                .unwrap_or(false);
            if is_hardlink {
                "hardlink".to_string()
            } else {
                default_mode.to_string()
            }
        })
        .collect()
}

/// Publish one reserved Transfer file: the reservation owns the file identity,
/// so this is a version-checked publish of the confirmed coverage of that same
/// file, never a second identity creation. The transfer mode a placed file
/// actually ended up with (a hardlink is only visible here), its source path and
/// the sidecar artwork beside it are written with the file.
pub(crate) fn publish_transfer_file(
    store: &store::Store,
    library_id: &str,
    config_revision: i64,
    coverage: &FileCoverage,
    source_path: Option<&std::path::Path>,
    observation: &TransferFileObservation,
) -> Result<domain::LibraryFile, StoreError> {
    let file_id = coverage.file.file_id;
    let observed = &coverage.file.version;
    let committed = store.get_file_version(file_id)?.ok_or_else(|| {
        StoreError::Protected("Transfer reservation changed or was detached".into())
    })?;
    if committed.revision != observed.revision
        || committed.source_version != observed.source_version
    {
        return Err(StoreError::Protected(
            "Transfer reservation changed or was detached".into(),
        ));
    }
    // The reservation already wrote this file's units; the collected coverage is
    // the fallback so a span is never narrowed back to its first episode. With
    // neither, the coverage is unknown and is published as such (never WHOLE).
    let stored_units = store.get_file_units(file_id)?;
    let units: Vec<domain::UnitKey> = if !stored_units.is_empty() {
        stored_units
    } else {
        coverage.units.clone()
    };
    let mut file = coverage.file.clone();
    let artwork = &observation.artwork;
    file.presence = domain::FilePresence::Present;
    file.publication = domain::FilePublication::Published;
    file.source_path = source_path.map(std::path::PathBuf::from);
    file.transfer_mode = Some(observation.mode.clone());
    file.poster_path = artwork.poster.clone();
    file.backdrop_path = artwork.backdrop.clone();
    file.still_path = artwork.still.clone();
    // Placement stat is the persisted evidence `bind_placed_file_source` checks
    // to keep facts probed from these very bytes.
    if let Some(observation) = observation.source.as_ref() {
        file.size_bytes = observation.size_bytes;
        file.modified_ns = observation.modified_ns;
        file.stat_known = true;
    }
    store.publish_files(
        &WriteScope {
            library_id: library_id.parse::<domain::LibraryId>()?,
            config_revision,
        },
        &[PublicationRequest {
            file: &file,
            units: &units,
            expected_version: Some(observed),
            nfo_observation: None,
            probe_required: false,
        }],
        store::unix_now() * 1000,
    )?;
    // The reservation version is a placeholder for bytes that did not exist yet;
    // the source observed (outside the Store lock) after placement advances it to
    // the real production encoding by CAS, so Probe/fingerprints bind real bytes.
    if let Some(observation) = observation.source.as_ref()
        && observation.source_version != observed.source_version
    {
        file.version = store.bind_placed_file_source(
            &WriteScope {
                library_id: library_id.parse::<domain::LibraryId>()?,
                config_revision,
            },
            file_id,
            observed,
            observation,
            store::unix_now() * 1000,
        )?;
    }
    tracing::info!(file_id = %file_id, path = %coverage.file.path.display(), "Published Transfer file");
    Ok(file)
}
