//! Claim publication pins DB context, observes disk without a Store guard, then CAS commits.

use std::path::Path;

use domain::{FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId, Media};
use store::library_write::{FilePublication as PublicationRequest, WriteScope};
use store::metadata::nfo_input::NfoInputObserver;
use store::{Store, StoreError};

use super::ClaimedFacts;
mod access;
use access::{Access, Error};

pub(super) struct ClaimPlanInputs {
    pub(super) source: store::UnidentifiedRow,
    pub(super) library_id: LibraryId,
    pub(super) config_revision: i64,
    pub(super) root: std::path::PathBuf,
    pub(super) mode: Option<library::TransferMode>,
    pub(super) scrape: bool,
    pub(super) mirror_nfo: bool,
    pub(super) naming: String,
}

pub(super) fn load_claim_plan_inputs(
    store: &Store,
    unidentified_path: &str,
    kind: domain::MediaKind,
) -> Result<ClaimPlanInputs, axum::response::Response> {
    use crate::scrape_store::ScrapeStoreExt;
    use axum::response::IntoResponse;
    let source = store
        .get_unidentified(unidentified_path)
        .map_err(super::store_response)?
        .ok_or_else(|| {
            crate::management::ApiError::missing(
                "unidentified.missing",
                "Unidentified not found".into(),
            )
            .into_response()
        })?;
    let library = store
        .default_library(kind)
        .map_err(super::store_response)?
        .ok_or_else(|| {
            crate::management::ApiError::missing(
                "library.missing",
                "Default library not found".into(),
            )
            .into_response()
        })?;
    let library_id: LibraryId = library.id.parse().map_err(|_| {
        crate::management::ApiError::invalid("unidentified.invalid", "invalid library id".into())
            .into_response()
    })?;
    let config_revision = store
        .library_config_revision(&library.id)
        .map_err(super::store_response)?;
    let (root, mode, scrape) =
        crate::directory::transfer_plan(store, kind).map_err(super::store_response)?;
    let mirror_nfo = scrape
        && store
            .get_scrape_config()
            .map_err(super::store_response)?
            .effective
            .mirror_nfo;
    let naming = store.naming_pattern(kind).map_err(super::store_response)?;
    Ok(ClaimPlanInputs {
        source,
        library_id,
        config_revision,
        root,
        mode,
        scrape,
        mirror_nfo,
        naming,
    })
}

#[derive(Clone, Copy)]
pub(super) struct PinnedClaimTarget<'a> {
    pub(super) library_id: LibraryId,
    pub(super) root_id: &'a str,
    pub(super) config_revision: i64,
}

pub(super) fn resolve_existing_ancestor_path(path: &Path) -> std::path::PathBuf {
    for anc in path.ancestors() {
        if anc.as_os_str().is_empty() {
            break;
        }
        if let Ok(canon) = std::fs::canonicalize(anc) {
            return match path.strip_prefix(anc) {
                Ok(rem) if !rem.as_os_str().is_empty() => canon.join(rem),
                _ => canon,
            };
        }
    }
    path.to_path_buf()
}

pub(super) fn verify_leased_target_in_store(
    store: &Store,
    lease: &crate::library_lifecycle::LibraryWorkLease,
    pinned: PinnedClaimTarget<'_>,
    dest: &Path,
    kind: domain::MediaKind,
) -> Result<(), axum::response::Response> {
    use axum::response::IntoResponse;
    let conflict = |msg: String| {
        crate::management::ApiError::with(
            axum::http::StatusCode::CONFLICT,
            "library.unavailable",
            msg,
        )
        .into_response()
    };
    lease
        .control()
        .check()
        .map_err(|e| conflict(e.to_string()))?;
    let lib_str = pinned.library_id.to_string();
    let resolved = resolve_existing_ancestor_path(dest);
    let ok = store
        .library_accepts_work(&lib_str)
        .and_then(|accepts| {
            Ok(accepts
                && store.library_lifecycle_state(&lib_str)? == "ready"
                && store.library_config_revision(&lib_str)? == pinned.config_revision
                && store.library_root_owning_key(&resolved, kind)?.as_ref()
                    == Some(&(lib_str, pinned.root_id.to_string())))
        })
        .map_err(super::store_response)?;
    if !ok {
        return Err(conflict(
            "Claim target Library changed before staging".into(),
        ));
    }
    Ok(())
}

pub(super) fn cleanup_staged_files(
    rollback_dest: Option<&Path>,
    destination_version: &str,
    sidecars: &[std::path::PathBuf],
    created_parent: Option<&Path>,
) {
    if let Some(path) = rollback_dest
        && library::current_source_version(path) == destination_version
        && let Err(error) = std::fs::remove_file(path)
    {
        tracing::warn!(path=%path.display(),%error,"Claim staged Copy cleanup failed");
    }
    for sidecar in sidecars {
        if let Err(error) = std::fs::remove_file(sidecar)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path=%sidecar.display(),%error,"Claim staged sidecar cleanup failed");
        }
    }
    if let Some(dir) = created_parent {
        let _ = std::fs::remove_dir(dir);
    }
}

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
        None,
    )
    .map_err(|error| match error {
        Error::Store(error) => error,
        Error::Admission(_) => StoreError::Protected("Claim Store is busy".into()),
    })
}

pub(super) fn publish_http_unidentified(
    state: &crate::ApiState,
    media: &Media,
    dest: &Path,
    facts: &ClaimedFacts,
    source: &store::UnidentifiedRow,
    destination_version: Option<&str>,
    pinned: Option<PinnedClaimTarget<'_>>,
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
        pinned,
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
    pinned: Option<PinnedClaimTarget<'_>>,
) -> Result<LibraryFile, Error> {
    let result = (|| {
        let path = std::fs::canonicalize(dest)?;
        let observed = library::observe_source(&path)
            .map_err(|error| StoreError::Io(std::io::Error::other(error)))?;
        if destination_version.is_some_and(|expected| expected != observed.source_version) {
            return Err(StoreError::Protected("staged Claim destination changed".into()).into());
        }
        let mut prepared = access
            .with_store(|store| prepare_claim(store, media, &path, &observed, facts, pinned))?;
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
        let outbox = if source.is_some() {
            let rel_path = crate::plugins::events::compute_relative_path(
                access.shared,
                prepared.scope.library_id,
                &path,
            );
            let now_ms = store::unix_now().saturating_mul(1_000);
            crate::plugins::events::build_transfer_completed_event(
                media.id,
                prepared.scope.library_id,
                None,
                rel_path,
                1,
                observed.size_bytes.unwrap_or(0),
                format!("claim:{}", prepared.file.file_id),
                now_ms,
            )
        } else {
            None
        };
        let now_ms = store::unix_now().saturating_mul(1_000);
        access.with_store(|store| {
            if let Some(source) = source {
                store.publish_unidentified_checked_context_with_outbox(
                    &prepared.scope,
                    &publication,
                    prepared.expected.as_ref(),
                    source,
                    now_ms,
                    outbox.as_ref(),
                )?;
            } else {
                store.publish_file_checked_context(
                    &prepared.scope,
                    &publication,
                    prepared.expected.as_ref(),
                    now_ms,
                )?;
            }
            Ok(())
        })?;
        if outbox.is_some()
            && let Some(st) = access.state
        {
            crate::plugins::events::wake(st);
        }
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
    pinned: Option<PinnedClaimTarget<'_>>,
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
    if let Some(pin) = pinned
        && (library_id != pin.library_id || root_id != pin.root_id)
    {
        return Err(StoreError::Protected(
            "claim target library or root changed before publication".into(),
        ));
    }
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
    if let Some(pin) = pinned
        && config_revision != pin.config_revision
    {
        return Err(StoreError::Protected(
            "claim target library config_revision changed before publication".into(),
        ));
    }
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

#[cfg(all(test, unix))]
mod tests {
    use std::sync::Arc;

    use domain::MediaKind;
    use downloader::MemoryDownloader;
    use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};

    use super::{PinnedClaimTarget, verify_leased_target_in_store};

    struct NoopFetcher;
    impl Fetcher for NoopFetcher {
        fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
            Ok(String::new())
        }
    }

    #[test]
    fn verify_leased_target_rejects_symlink_escape_nested_root_and_revision_drift() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::Store::open(tmp.path().join("data")).unwrap();
        let state = crate::ApiState::new(
            store,
            "tok".into(),
            RegistrySnapshot::builtin().unwrap(),
            Arc::new(NoopFetcher),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .unwrap();
        let s = state.store.as_ref();
        let movie = s.default_library(MediaKind::Movie).unwrap().unwrap();
        let lib_id: domain::LibraryId = movie.id.parse().unwrap();
        let root = movie.root_paths[0].clone();
        let (_, root_id) = s
            .library_root_owning_key(&root.join("probe.mkv"), MediaKind::Movie)
            .unwrap()
            .unwrap();
        let lease = state.acquire_library_work(&movie.id).unwrap();
        let pin = PinnedClaimTarget {
            library_id: lib_id,
            root_id: &root_id,
            config_revision: 0,
        };

        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("sym")).unwrap();
        assert!(
            verify_leased_target_in_store(
                s,
                &lease,
                pin,
                &root.join("sym/Film.mkv"),
                MediaKind::Movie
            )
            .is_err()
        );

        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        s.create_library(
            MediaKind::Movie,
            "Nested",
            &[nested.to_str().unwrap()],
            "everyone",
            false,
            &[],
        )
        .unwrap();
        assert!(
            verify_leased_target_in_store(
                s,
                &lease,
                pin,
                &nested.join("Film.mkv"),
                MediaKind::Movie
            )
            .is_err()
        );

        let stale_pin = PinnedClaimTarget {
            library_id: lib_id,
            root_id: &root_id,
            config_revision: 7,
        };
        assert!(
            verify_leased_target_in_store(
                s,
                &lease,
                stale_pin,
                &root.join("Safe.mkv"),
                MediaKind::Movie
            )
            .is_err()
        );
    }
}
