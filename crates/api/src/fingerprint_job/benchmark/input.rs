use super::*;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};

pub(super) struct VerifiedInput {
    pub case: BenchmarkCase,
    pub source: BenchmarkSource,
    pub error: Option<String>,
}

pub(super) fn load(
    options: &BenchmarkOptions,
) -> Result<(BenchmarkManifest, Vec<VerifiedInput>), String> {
    let manifest: BenchmarkManifest = read_json(&options.manifest)?;
    let sources: Vec<BenchmarkSource> = read_json(&options.sources)?;
    validate(&manifest, &sources)?;
    let mut verified = BTreeMap::new();
    let mut inputs = Vec::new();
    for case in &manifest.cases {
        let source = sources
            .iter()
            .find(|s| s.case_id == case.case_id)
            .ok_or("source missing")?
            .clone();
        let error = verified
            .entry(source.path.clone())
            .or_insert_with(|| {
                let is_strm = source
                    .path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("strm"));
                if is_strm {
                    return Some(
                        if options.allow_remote {
                            "remote_source_content_unverified"
                        } else {
                            "remote_source_requires_allow_remote"
                        }
                        .into(),
                    );
                }
                match hash_file(&source.path) {
                    Ok(hash) if hash == case.source_hash => None,
                    Ok(_) => Some("source_hash_mismatch".into()),
                    Err(error) => Some(format!("source_read_failed: {error}")),
                }
            })
            .clone();
        inputs.push(VerifiedInput {
            case: case.clone(),
            source,
            error,
        });
    }
    Ok((manifest, inputs))
}

fn validate(manifest: &BenchmarkManifest, sources: &[BenchmarkSource]) -> Result<(), String> {
    if manifest.corpus_version.trim().is_empty()
        || !matches!(
            manifest.evidence_kind.as_str(),
            "synthetic" | "human_reviewed" | "public_annotation"
        )
        || manifest.cases.is_empty()
    {
        return Err("invalid manifest provenance or empty corpus".into());
    }
    let mut cases = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    let mut source_identity = BTreeMap::new();
    let mut group_splits = BTreeMap::new();
    for case in &manifest.cases {
        if case.case_id.trim().is_empty()
            || case.media_group.trim().is_empty()
            || !cases.insert(&case.case_id)
            || !matches!(case.kind.as_str(), "intro" | "outro")
            || !matches!(
                case.split.as_str(),
                "development" | "validation" | "holdout"
            )
            || case.duration_ms <= 0
            || !valid_hash(&case.source_hash)
            || !kinds.insert((&case.source_hash, &case.kind))
        {
            return Err("invalid or duplicate manifest case".into());
        }
        let identity = (&case.media_group, &case.split, case.duration_ms);
        if source_identity
            .insert(&case.source_hash, identity)
            .is_some_and(|old| old != identity)
            || group_splits
                .insert(&case.media_group, &case.split)
                .is_some_and(|old| old != &case.split)
        {
            return Err("inconsistent source or Media split leakage".into());
        }
    }
    let mut ids = BTreeSet::new();
    let mut paths = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    for source in sources {
        let case = manifest
            .cases
            .iter()
            .find(|case| case.case_id == source.case_id)
            .ok_or("unexpected source mapping")?;
        if !ids.insert(&source.case_id) || !source.path.is_absolute() {
            return Err("duplicate mapping or non-absolute source path".into());
        }
        let unit = domain::UnitKey::new(source.unit.season, source.unit.episode);
        if unit.is_whole() {
            return Err("unsupported whole-work fingerprint evaluation".into());
        }
        if source.unit.season < 0 || source.unit.episode < 0 {
            return Err("invalid episode coordinates".into());
        }
        let identity = (
            &case.source_hash,
            &case.media_group,
            source.unit,
            source.audio_stream_index,
        );
        if paths
            .insert(&source.path, identity)
            .is_some_and(|old| old != identity)
        {
            return Err("same source mappings disagree on hash/audio/unit/Media".into());
        }
        let identity = (
            &source.path,
            &case.media_group,
            source.unit,
            source.audio_stream_index,
        );
        if hashes
            .insert(&case.source_hash, identity)
            .is_some_and(|old| old != identity)
        {
            return Err("same source hash mappings disagree".into());
        }
    }
    if ids.len() != cases.len() {
        return Err("every case requires a source mapping".into());
    }
    Ok(())
}

fn valid_hash(hash: &str) -> bool {
    hash.strip_prefix("sha256:")
        .is_some_and(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()))
}
pub(super) fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}
pub(super) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}
pub(super) fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

pub(super) fn evaluation_directory(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("evaluation data directory must be explicit and absolute".into());
    }
    let marker = path.join(".fingerprint-evaluation.json");
    let expected = serde_json::json!({"kind":"crawler-media-fingerprint-evaluation","version":1});
    if path.exists() {
        if marker.exists() {
            let actual: serde_json::Value = read_json(&marker)?;
            if actual != expected {
                return Err("evaluation directory marker mismatch".into());
            }
            return Ok(());
        }
        if std::fs::read_dir(path)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
        {
            return Err("refusing existing non-evaluation instance".into());
        }
    }
    std::fs::create_dir_all(path).map_err(|e| e.to_string())?;
    write_json(&marker, &expected)
}
