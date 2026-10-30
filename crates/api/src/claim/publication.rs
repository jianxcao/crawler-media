//! Claim publication pins DB context, observes disk without a Store guard, then CAS commits.

use std::path::Path;

use domain::{FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId, Media};
use store::library_write::{FilePublication as PublicationRequest, WriteScope};
use store::metadata::nfo_input::NfoInputObserver;
use store::{Store, StoreError};

use super::ClaimedFacts;
mod access;
use access::{Access, Error};

struct PreparedClaim {
    file: LibraryFile,
    expected: Option<LibraryFile>,
    scope: WriteScope,
    locale: String,
    observer: NfoInputObserver,
}

/// Test fixture publication shares the production context/CAS writer.
#[cfg(test)]
pub(crate) fn publish_claimed_file(
    shared: &Store,
    media: &Media,
    dest: &Path,
    facts: &ClaimedFacts,
) -> Result<LibraryFile, StoreError> {
    publish_claim(
        &Access {
            shared,
            state: None,
        },
        media,
        dest,
        facts,
        None,
        None,
    )
    .map_err(|error| match error {
        Error::Store(error) => error,
        Error::Admission(_) => StoreError::Protected("Claim Store is busy".into()),
    })
}

pub(crate) fn publish_http_unidentified(
    state: &crate::ApiState,
    media: &Media,
    dest: &Path,
    facts: &ClaimedFacts,
    source: &store::UnidentifiedRow,
    destination_version: Option<&str>,
) -> Result<LibraryFile, axum::response::Response> {
    publish_claim(
        &Access {
            shared: &state.store,
            state: Some(state),
        },
        media,
        dest,
        facts,
        Some(source),
        destination_version,
    )
    .map_err(Error::response)
}

fn publish_claim(
    access: &Access<'_>,
    media: &Media,
    dest: &Path,
    facts: &ClaimedFacts,
    source: Option<&store::UnidentifiedRow>,
    destination_version: Option<&str>,
) -> Result<LibraryFile, Error> {
    let result = (|| {
        let path = std::fs::canonicalize(dest)?;
        let observed = library::observe_source(&path)
            .map_err(|error| StoreError::Io(std::io::Error::other(error)))?;
        if destination_version.is_some_and(|expected| expected != observed.source_version) {
            return Err(StoreError::Protected("staged Claim destination changed".into()).into());
        }
        let mut prepared =
            access.with_store(|store| prepare_claim(store, media, &path, &observed, facts))?;
        let observations = prepared.observer.observe(
            &path,
            media.kind,
            &facts.units,
            &prepared.locale,
            &prepared.file.version,
            prepared.scope.config_revision,
        );
        let artwork = store::files::DiscoveredArtwork::discover(&path);
        prepared.file.poster_path = artwork.poster;
        prepared.file.backdrop_path = artwork.backdrop;
        prepared.file.still_path = artwork.still;
        if library::current_source_version(&path) != prepared.file.version.source_version {
            return Err(
                StoreError::Protected("claim source changed during observation".into()).into(),
            );
        }
        let publication = PublicationRequest {
            file: &prepared.file,
            units: &facts.units,
            expected_version: Some(&prepared.file.version),
            nfo_observation: Some(&observations),
            probe_required: false,
        };
        access.with_store(|store| {
            if let Some(source) = source {
                store.publish_unidentified_checked_context(
                    &prepared.scope,
                    &publication,
                    prepared.expected.as_ref(),
                    source,
                    store::unix_now() * 1000,
                )?;
            } else {
                store.publish_file_checked_context(
                    &prepared.scope,
                    &publication,
                    prepared.expected.as_ref(),
                    store::unix_now() * 1000,
                )?;
            }
            Ok(())
        })?;
        tracing::info!(file_id = %prepared.file.file_id, path = %path.display(), "Published claimed file");
        Ok(prepared.file)
    })();
    if let Err(error) = &result {
        tracing::error!(path = %dest.display(), error = ?match error { Error::Store(error) => error.to_string(), Error::Admission(response) => response.status().to_string() }, "Claim publication rejected");
    }
    result
}

fn prepare_claim(
    store: &Store,
    media: &Media,
    path: &Path,
    source: &library::SourceObservation,
    facts: &ClaimedFacts,
) -> Result<PreparedClaim, StoreError> {
    let (owner, root_id) = store
        .library_root_owning_key(path, media.kind)?
        .ok_or_else(|| {
            StoreError::Invalid(format!(
                "path is outside every Library root: {}",
                path.display()
            ))
        })?;
    let library_id: LibraryId = owner.parse()?;
    if !store.library_accepts_work(&owner)? || store.library_lifecycle_state(&owner)? != "ready" {
        return Err(StoreError::Protected(format!(
            "Library {owner} does not accept claim work"
        )));
    }
    let existing = store.file_coverage_by_path(&path.to_string_lossy())?;
    let expected = existing.as_ref().map(|row| row.file.clone());
    let mut file = match existing {
        Some(row) => {
            let file = row.file;
            if file.library_id != library_id
                || file.root_id != root_id
                || file.media_id != media.id
                || file.version.source_version != source.source_version
            {
                return Err(StoreError::Protected(
                    "claim file identity or source is stale".into(),
                ));
            }
            file
        }
        None => new_file(media, path, library_id, root_id, source, facts),
    };
    file.path = path.to_path_buf();
    file.presence = FilePresence::Present;
    file.publication = FilePublication::Published;
    file.resolution = facts.resolution.clone();
    file.codec = facts.codec.clone();
    file.hdr = facts.hdr.clone();
    file.quality_source = facts.quality_source;
    file.confidence = facts.confidence;
    file.filter_score = facts.filter_score;
    let config_revision = store.library_config_revision(&owner)?;
    let locale =
        store::library_query::library_display_locale(store, library_id)?.unwrap_or_default();
    Ok(PreparedClaim {
        file,
        expected,
        scope: WriteScope {
            library_id,
            config_revision,
        },
        locale,
        observer: store.nfo_input_observer(),
    })
}

fn new_file(
    media: &Media,
    path: &Path,
    library_id: LibraryId,
    root_id: String,
    source: &library::SourceObservation,
    facts: &ClaimedFacts,
) -> LibraryFile {
    LibraryFile {
        file_id: LedgerId::new(),
        library_id,
        root_id,
        media_id: media.id,
        kind: media.kind,
        path: path.to_path_buf(),
        version: FileVersion {
            revision: 1,
            source_version: source.source_version.clone(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: source.size_bytes,
        stat_known: true,
        created_ns: source.created_ns,
        modified_ns: source.modified_ns,
        source_digest: source.content_digest.clone(),
        source_path: None,
        transfer_mode: None,
        resolution: facts.resolution.clone(),
        codec: facts.codec.clone(),
        hdr: facts.hdr.clone(),
        quality_source: facts.quality_source,
        confidence: facts.confidence,
        filter_score: facts.filter_score,
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}
