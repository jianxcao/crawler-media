//! NFO inputs are observed outside the Store mutex and published with optimistic CAS.

use std::path::{Path, PathBuf};

use domain::{EntityScope, LibraryFile, MediaKind, MetadataFields};
use library::nfo::{NfoReadOutcome, parse_nfo_fields};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::StoreError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NfoParseState { Valid, Invalid, Unknown, Missing }

/// Owned NFO observation produced before acquiring the Store mutex.
#[derive(Clone, Debug, PartialEq)]
pub struct NfoObservation {
    pub input_key: String,
    pub sidecar_path: Option<PathBuf>,
    pub locale: String,
    pub expected_source_version: String,
    pub expected_config_revision: i64,
    pub scope: EntityScope,
    pub state: NfoParseState,
    pub digest: Option<String>,
    pub fields: MetadataFields,
    pub provenance_json: String,
}

impl NfoObservation {
    pub fn observe(path: &Path, kind: MediaKind, locale: String, source_version: String, config_revision: i64) -> Self {
        for candidate in nfo_candidates(path, kind) {
            match std::fs::read(&candidate) {
                Ok(bytes) => {
                    let digest = format!("{:x}", Sha256::digest(&bytes));
                    let document = match std::str::from_utf8(&bytes) {
                        Ok(body) => roxmltree::Document::parse(body).ok(),
                        Err(_) => None,
                    };
                    let scope = match document.as_ref().map(|doc| doc.root_element().tag_name().name()) {
                        Some("season") => season_from_path(path).map(EntityScope::Season).unwrap_or(EntityScope::Work),
                        Some("episodedetails") => match (season_from_path(path), episode_from_path(path)) {
                            (Some(season), Some(episode)) => EntityScope::Episode { season, episode },
                            _ => EntityScope::Work,
                        },
                        _ => EntityScope::Work,
                    };
                    let (state, fields) = match std::str::from_utf8(&bytes) {
                        Ok(body) => match parse_nfo_fields(body, scope) {
                            NfoReadOutcome::Valid(input) => (NfoParseState::Valid, input.fields),
                            NfoReadOutcome::Invalid { .. } => (NfoParseState::Invalid, MetadataFields::default()),
                            NfoReadOutcome::Unknown { .. } => (NfoParseState::Unknown, MetadataFields::default()),
                            NfoReadOutcome::Missing => (NfoParseState::Missing, MetadataFields::default()),
                        },
                        Err(_) => (NfoParseState::Unknown, MetadataFields::default()),
                    };
                    return Self::new(path, Some(candidate), locale, source_version, config_revision, scope, state, Some(digest), fields);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Self::new(path, Some(candidate), locale, source_version, config_revision, EntityScope::Work,
                    NfoParseState::Unknown, None, MetadataFields::default()),
            }
        }
        Self::new(path, None, locale, source_version, config_revision, EntityScope::Work, NfoParseState::Missing, None, MetadataFields::default())
    }

    fn new(path: &Path, sidecar_path: Option<PathBuf>, locale: String, source_version: String, config_revision: i64,
        scope: EntityScope, state: NfoParseState, digest: Option<String>, fields: MetadataFields) -> Self {
        Self { input_key: path.to_string_lossy().into_owned(), sidecar_path, locale,
            expected_source_version: source_version, expected_config_revision: config_revision, scope, state, digest, fields,
            provenance_json: r#"{"kind":"nfo","authority":"library_sidecar"}"#.into() }
    }
}

fn season_from_path(path: &Path) -> Option<u32> { release::parse(path.file_name()?.to_str()?).season }
fn episode_from_path(path: &Path) -> Option<u32> { release::parse(path.file_name()?.to_str()?).episode }

fn nfo_candidates(path: &Path, kind: MediaKind) -> Vec<PathBuf> {
    let directory = path.parent().unwrap_or(path);
    let stem = path.file_stem().and_then(|value| value.to_str()).unwrap_or_default();
    match kind {
        MediaKind::Tv => {
            let season_dir = directory.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.to_ascii_lowercase().starts_with("season"));
            let series = if season_dir { directory.parent().unwrap_or(directory) } else { directory };
            vec![path.with_extension("nfo"), directory.join("season.nfo"), series.join("tvshow.nfo")]
        }
        _ => vec![path.with_extension("nfo"), directory.join(format!("{stem}.nfo")), directory.join("movie.nfo")],
    }
}

/// Publishes an observation only while its file source and Library config revisions match.
/// Returns false for stale observations; callers must reobserve outside the transaction.
pub fn record_nfo_observation(tx: &Connection, file: &LibraryFile, observation: &NfoObservation) -> Result<bool, StoreError> {
    if file.version.source_version != observation.expected_source_version { return Ok(false); }
    let config_revision: Option<i64> = tx.query_row("SELECT config_revision FROM libraries WHERE library_id=?1", params![file.library_id.to_string()], |row| row.get(0)).optional()?;
    if config_revision != Some(observation.expected_config_revision) { return Ok(false); }
    let current_source: Option<String> = tx.query_row("SELECT source_version FROM library_files WHERE file_id=?1 AND library_id=?2", params![file.file_id.to_string(), file.library_id.to_string()], |row| row.get(0)).optional()?;
    if current_source.as_deref() != Some(observation.expected_source_version.as_str()) { return Ok(false); }
    let (entity, season, episode) = (observation.scope.as_str(), observation.scope.season_episode().0, observation.scope.season_episode().1);
    if matches!(observation.state, NfoParseState::Invalid | NfoParseState::Unknown) { return Ok(true); }
    if observation.state == NfoParseState::Missing {
        tx.execute("DELETE FROM library_metadata_inputs WHERE library_id=?1 AND media_id=?2 AND entity_scope=?3 AND season=?4 AND episode=?5 AND locale=?6 AND input_key=?7",
            params![file.library_id.to_string(), file.media_id.to_string(), entity, season, episode, observation.locale, observation.input_key])?;
        return Ok(true);
    }
    let fields_json = serde_json::to_string(&observation.fields)?;
    let present: Vec<String> = serde_json::to_value(&observation.fields)?.as_object().map(|object|
        object.iter().filter(|(_, value)| value.get("state").and_then(|state| state.as_str()) != Some("missing")).map(|(name, _)| name.clone()).collect()).unwrap_or_default();
    let present_json = serde_json::to_string(&present)?;
    let path_key = file.path.to_string_lossy();
    let root_exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM library_roots WHERE root_id=?1 AND library_id=?2)", params![file.root_id, file.library_id.to_string()], |row| row.get(0))?;
    if !root_exists { return Ok(false); }
    tx.execute("INSERT INTO library_metadata_inputs (library_id,media_id,entity_scope,season,episode,locale,input_key,root_id,path_key,parse_state,source_version,fields_json,present_fields_json,provenance_json,revision) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'valid','nfo_v1',?10,?11,?12,1) ON CONFLICT(library_id,media_id,entity_scope,season,episode,locale,input_key) DO UPDATE SET root_id=excluded.root_id,path_key=excluded.path_key,parse_state='valid',source_version=excluded.source_version,fields_json=excluded.fields_json,present_fields_json=excluded.present_fields_json,provenance_json=excluded.provenance_json,revision=library_metadata_inputs.revision+1",
        params![file.library_id.to_string(), file.media_id.to_string(), entity, season, episode, observation.locale, observation.input_key,
            file.root_id, path_key, fields_json, present_json, observation.provenance_json])?;
    Ok(true)
}


