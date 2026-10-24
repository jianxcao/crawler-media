use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId,
    MediaId, QualitySource, UnitKey,
};
use rusqlite::OptionalExtension;
use std::path::Path;

use super::{Store, StoreError};
use crate::library_write::{FilePublication as PublicationRequest, WriteScope};

/// Specification of a file to be published in tests: the ledger facts a fixture
/// needs, expressed with the same shapes the production read model uses.
#[derive(Clone, Debug, PartialEq)]
pub struct FixtureFile {
    pub media_id: MediaId,
    pub path: String,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub resolution: Option<String>,
    pub codec: Option<String>,
    pub hdr: Option<String>,
    pub quality_source: QualitySource,
    pub confidence: Confidence,
    pub filter_score: Option<i32>,
    pub transfer_mode: Option<String>,
    pub source_path: Option<String>,
    pub file_id: Option<LedgerId>,
}

impl Default for FixtureFile {
    fn default() -> Self {
        Self {
            media_id: MediaId::new(),
            path: String::new(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
            transfer_mode: None,
            source_path: None,
            file_id: None,
        }
    }
}

/// Publish a single fixture file to the store through the canonical publication path.
pub fn publish_fixture_file(store: &Store, spec: &FixtureFile) -> Result<LibraryFile, StoreError> {
    let published = publish_fixture_files(store, std::slice::from_ref(spec))?;
    published
        .into_iter()
        .next()
        .ok_or_else(|| StoreError::Invalid("expected published file".into()))
}

/// Publish multiple fixture files grouped by library and write scope.
pub fn publish_fixture_files(
    store: &Store,
    specs: &[FixtureFile],
) -> Result<Vec<LibraryFile>, StoreError> {
    if specs.is_empty() {
        return Ok(Vec::new());
    }

    let mut results = Vec::with_capacity(specs.len());
    for spec in specs {
        results.push(publish_with_units(store, spec, &fixture_units(spec))?);
    }
    Ok(results)
}

/// Publish one fixture file with an explicit coverage (e.g. a merged E01+E02
/// episode file). `spec.season/episode` are ignored.
pub fn publish_fixture_file_units(
    store: &Store,
    spec: &FixtureFile,
    units: &[UnitKey],
) -> Result<LibraryFile, StoreError> {
    publish_with_units(store, spec, units)
}

fn publish_with_units(
    store: &Store,
    spec: &FixtureFile,
    units: &[UnitKey],
) -> Result<LibraryFile, StoreError> {
    {
        let file = place_fixture_file(store, spec)?;
        let units = units.to_vec();
        let locale = crate::library_query::library_display_locale(store, file.library_id)
            .ok()
            .flatten()
            .unwrap_or_default();
        let config_revision = store
            .library_config_revision(&file.library_id.to_string())
            .unwrap_or(0);
        let observations = store.nfo_input_observer().observe(
            &file.path,
            file.kind,
            &units,
            &locale,
            &file.version,
            config_revision,
        );

        store.publish_files(
            &WriteScope {
                library_id: file.library_id,
                config_revision,
            },
            &[PublicationRequest {
                file: &file,
                units: &units,
                expected_version: None,
                nfo_observation: Some(&observations),
                probe_required: false,
            }],
            crate::unix_now() * 1000,
        )?;

        Ok(file)
    }
}

fn fixture_units(spec: &FixtureFile) -> Vec<UnitKey> {
    match (spec.season, spec.episode) {
        (Some(season), Some(episode)) => vec![UnitKey::new(season as i32, episode as i32)],
        _ => vec![UnitKey::WHOLE],
    }
}

fn place_fixture_file(store: &Store, spec: &FixtureFile) -> Result<LibraryFile, StoreError> {
    let media = store
        .get_media(spec.media_id)?
        .ok_or_else(|| StoreError::Missing(format!("media {}", spec.media_id)))?;
    let library = store
        .library_for_path_strict(Path::new(&spec.path), media.kind)?
        .ok_or_else(|| {
            StoreError::Invalid(format!("path is outside every Library root: {}", spec.path))
        })?;
    let root_id = root_for_path(store, &library.id, &spec.path)?;
    let existing: Option<(String, i64, String)> = store
        .library
        .query_row(
            "SELECT file_id, file_revision, source_version FROM library_files
             WHERE path_key = norm_path_key(?1)
             LIMIT 1",
            [&spec.path],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;

    let (file_id, revision, source_version) = match existing {
        Some((file_id, revision, source_version)) => {
            (file_id.parse::<LedgerId>()?, revision, source_version)
        }
        None => {
            let id = spec.file_id.unwrap_or_else(LedgerId::new);
            // Files that exist on disk carry the production content token, so
            // fixture rows behave like scanned ones (rename, Probe, CAS).
            let source_version = library::observe_source(Path::new(&spec.path))
                .map(|observation| observation.source_version)
                .unwrap_or_else(|_| format!("{}-{}", crate::unix_now(), id));
            (id, 1, source_version)
        }
    };

    let artwork = crate::files::DiscoveredArtwork::discover(Path::new(&spec.path));
    Ok(LibraryFile {
        file_id,
        library_id: library.id.parse::<LibraryId>()?,
        root_id,
        media_id: spec.media_id,
        kind: media.kind,
        path: spec.path.clone().into(),
        version: FileVersion {
            revision,
            source_version,
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: None,
        stat_known: false,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: spec.source_path.clone().map(Into::into),
        transfer_mode: spec.transfer_mode.clone(),
        resolution: spec.resolution.clone(),
        codec: spec.codec.clone(),
        hdr: spec.hdr.clone(),
        quality_source: spec.quality_source,
        confidence: spec.confidence,
        filter_score: spec.filter_score,
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: artwork.poster,
        backdrop_path: artwork.backdrop,
        still_path: artwork.still,
    })
}

fn root_for_path(store: &Store, library_id: &str, path: &str) -> Result<String, StoreError> {
    let resolved = crate::library_paths::resolved_path(Path::new(path))?;
    let resolved_key = crate::path_key::normalize_path_key(&resolved.to_string_lossy());
    let mut best: Option<(usize, String)> = None;
    for root in store.list_library_roots()? {
        if root.library_id.as_deref() != Some(library_id) {
            continue;
        }
        let normalized = crate::library_paths::resolved_path(&root.path)?;
        let root_key = crate::path_key::normalize_path_key(&normalized.to_string_lossy());
        if !(resolved_key == root_key || resolved_key.starts_with(&(root_key.clone() + "/"))) {
            continue;
        }
        let depth = Path::new(&root_key).components().count();
        if best
            .as_ref()
            .is_none_or(|(best_depth, _)| *best_depth < depth)
        {
            best = Some((depth, root.id));
        }
    }
    best.map(|(_, id)| id)
        .ok_or_else(|| StoreError::Invalid(format!("path is outside every Library root: {path}")))
}

/// Force a Media identity state (e.g. `merging`) to exercise freeze guards.
pub fn set_media_identity_state(
    store: &Store,
    media_id: MediaId,
    state: &str,
) -> Result<(), StoreError> {
    store.library.execute(
        "UPDATE media SET identity_state = ?2 WHERE media_id = ?1",
        rusqlite::params![media_id.to_string(), state],
    )?;
    Ok(())
}

/// Overwrite a recorded path without any ownership checks, to simulate a
/// legacy/corrupt row in tests. Production renames use
/// [`Store::rename_file_within_root`].
pub fn force_file_path(store: &Store, from: &str, to: &str) -> Result<bool, StoreError> {
    let n = store.library.execute(
        "UPDATE library_files SET path_key = ?2, display_path = ?2
         WHERE path_key = norm_path_key(?1)",
        rusqlite::params![from, to],
    )?;
    Ok(n > 0)
}

/// Force a file's presence/publication to exercise read scopes.
pub fn force_file_states(
    store: &Store,
    file_id: LedgerId,
    presence: FilePresence,
    publication: FilePublication,
) -> Result<(), StoreError> {
    store.library.execute(
        "UPDATE library_files SET presence_state = ?2, publication_state = ?3 WHERE file_id = ?1",
        rusqlite::params![file_id.to_string(), presence.as_str(), publication.as_str()],
    )?;
    Ok(())
}

/// Allow only `budget` more root-reassignment batches on this thread (`None` = unbounded);
/// the remaining work stays in `pending_root_reassignments`, as after a crash.
pub fn set_root_reassign_batch_budget(budget: Option<usize>) {
    crate::roots::set_batch_budget(budget);
}

/// Pending root reassignment cursors, as `(kind, root_key, cursor_key)`.
pub fn pending_root_reassignments(store: &Store) -> Vec<(String, String, String)> {
    let mut stmt = store
        .library
        .prepare("SELECT kind, root_key, cursor_key FROM pending_root_reassignments")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

/// Number of ledger files owned by `library_id`.
pub fn library_file_count(store: &Store, library_id: &str) -> usize {
    store
        .library
        .query_row(
            "SELECT COUNT(*) FROM library_files WHERE library_id = ?1",
            [library_id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap() as usize
}
