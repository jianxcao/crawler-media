use super::input::VerifiedInput;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};
use store::library_write::{FilePublication, WriteScope};

#[derive(Clone)]
pub(super) struct RegisteredSource {
    pub original_path: std::path::PathBuf,
    pub row: domain::FileCoverage,
    pub media_group: String,
    pub audio_stream_index: u32,
    pub is_new: bool,
}

pub(super) fn register(
    store: &crate::Store,
    inputs: &mut [VerifiedInput],
    data_dir: &Path,
) -> Result<Vec<RegisteredSource>, String> {
    let root = data_dir.join("media");
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let library = match store
        .get_setting("benchmark.library_id")
        .map_err(|e| e.to_string())?
    {
        Some(id) => store
            .get_library(&id)
            .map_err(|e| e.to_string())?
            .ok_or("evaluation Library missing")?,
        None => {
            let library = store
                .create_library(
                    domain::MediaKind::Tv,
                    "Fingerprint evaluation",
                    &[root.to_str().ok_or("non-UTF8 evaluation path")?],
                    "admin",
                    true,
                    &[],
                )
                .map_err(|e| e.to_string())?;
            store
                .put_setting("benchmark.library_id", &library.id)
                .map_err(|e| e.to_string())?;
            store
                .set_library_intro_settings(&library.id, true, true)
                .map_err(|e| e.to_string())?;
            store
                .set_library_switch_settings(
                    &library.id,
                    Some(false),
                    Some(false),
                    Some(true),
                    Some(true),
                    Some(false),
                )
                .map_err(|e| e.to_string())?;
            library
        }
    };
    let library_id = library.id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let library_root = store
        .list_library_roots()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|r| r.library_id == library.id)
        .ok_or("evaluation root missing")?;
    let scope = WriteScope {
        library_id,
        config_revision: store
            .library_config_revision(&library.id)
            .map_err(|e| e.to_string())?,
    };
    let mut registered = BTreeMap::new();
    for input in inputs.iter_mut().filter(|input| input.error.is_none()) {
        let source = &input.source;
        if registered.contains_key(&source.path) {
            continue;
        }
        let result = register_one(store, input, &root, &library_root.id, &scope);
        match result {
            Ok(row) => {
                registered.insert(source.path.clone(), row);
            }
            Err(error) => input.error = Some(error),
        }
    }
    for input in inputs.iter_mut() {
        if input.error.is_none() && !registered.contains_key(&input.source.path) {
            input.error = Some("source registration failed".into());
        }
    }
    Ok(registered.into_values().collect())
}

fn register_one(
    store: &crate::Store,
    input: &VerifiedInput,
    root: &Path,
    root_id: &str,
    scope: &WriteScope,
) -> Result<RegisteredSource, String> {
    let source = &input.source;
    let media_id = ensure_media(store, &input.case.media_group)?;
    let file_id = domain::LedgerId::from_uuid(stable_id(&[
        &input.case.media_group,
        source.path.to_str().ok_or("non-UTF8 source path")?,
        &source.unit.season.to_string(),
        &source.unit.episode.to_string(),
    ]));
    let extension = source
        .path
        .extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| ext.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or("media");
    let path = root.join(format!("{file_id}.{extension}"));
    if !path.exists() {
        std::fs::hard_link(&source.path, &path)
            .or_else(|_| std::fs::copy(&source.path, &path).map(|_| ()))
            .map_err(|e| e.to_string())?;
    }
    if super::input::hash_file(&path)? != input.case.source_hash {
        return Err("isolated source hash mismatch".into());
    }
    let existing = store
        .file_coverage_by_id(file_id)
        .map_err(|e| e.to_string())?;
    if let Some(row) = existing {
        if row.units
            != [domain::UnitKey::new(
                source.unit.season,
                source.unit.episode,
            )]
        {
            return Err("existing source coordinates changed".into());
        }
        return Ok(RegisteredSource {
            original_path: source.path.clone(),
            row,
            media_group: input.case.media_group.clone(),
            audio_stream_index: source.audio_stream_index,
            is_new: false,
        });
    }
    let observed = library::observe_source(&path).map_err(|e| e.to_string())?;
    let file = domain::LibraryFile {
        file_id,
        library_id: scope.library_id,
        root_id: root_id.into(),
        media_id,
        kind: domain::MediaKind::Tv,
        path,
        version: domain::FileVersion {
            revision: 1,
            source_version: observed.source_version,
        },
        presence: domain::FilePresence::Present,
        publication: domain::FilePublication::Published,
        size_bytes: None,
        stat_known: false,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: domain::QualitySource::Probe,
        confidence: domain::Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    store
        .publish_files(
            scope,
            &[FilePublication {
                file: &file,
                units: &[domain::UnitKey::new(
                    source.unit.season,
                    source.unit.episode,
                )],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            0,
        )
        .map_err(|e| e.to_string())?;
    let row = store
        .file_coverage_by_id(file_id)
        .map_err(|e| e.to_string())?
        .ok_or("registered source missing")?;
    Ok(RegisteredSource {
        original_path: source.path.clone(),
        row,
        media_group: input.case.media_group.clone(),
        audio_stream_index: source.audio_stream_index,
        is_new: true,
    })
}

fn ensure_media(store: &crate::Store, group: &str) -> Result<domain::MediaId, String> {
    let media_id = domain::MediaId::from_uuid(stable_id(&[group]));
    if store
        .get_media(media_id)
        .map_err(|e| e.to_string())?
        .is_none()
    {
        store
            .insert_media(&domain::Media {
                id: media_id,
                kind: domain::MediaKind::Tv,
                title: group.into(),
                year: None,
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(media_id)
}

fn stable_id(parts: &[&str]) -> uuid::Uuid {
    let mut hash = Sha256::new();
    hash.update(b"fingerprint-evaluation-identity-v1\0");
    for part in parts {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    let bytes = hash.finalize();
    let mut id = [0; 16];
    id.copy_from_slice(&bytes[..16]);
    uuid::Uuid::from_bytes(id)
}
