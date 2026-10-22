use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;

use domain::{
    Confidence, Coverage, FileCoverage, FilePresence, FilePublication, FileVersion, FilterAtom,
    LedgerId, LibraryFile, Media, MediaKind, QualitySource, Site, SiteId, Subscribe, Torrent,
    UnitKey,
};

use super::StoreError;

pub(super) fn parse_id<T: FromStr>(raw: String, idx: usize) -> rusqlite::Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    T::from_str(&raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(err))
    })
}

pub(super) fn parse_token<T: FromStr>(raw: String, idx: usize) -> rusqlite::Result<T>
where
    T::Err: std::fmt::Display,
{
    T::from_str(&raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(
            idx,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                err.to_string(),
            )),
        )
    })
}

fn aliases_from_json(raw: Option<String>) -> Vec<String> {
    let Some(raw) = raw.filter(|raw| !raw.is_empty()) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub(super) fn aliases_json(aliases: &[String]) -> String {
    serde_json::to_string(aliases).unwrap_or_else(|_| "[]".to_string())
}

pub(super) fn map_media(row: &rusqlite::Row<'_>) -> rusqlite::Result<Media> {
    let kind: String = row.get(1)?;
    Ok(Media {
        id: parse_id(row.get(0)?, 0)?,
        kind: parse_token(kind, 1)?,
        title: row.get(2)?,
        year: row.get::<_, Option<i64>>(3)?.map(|v| v as u16),
        original_title: row.get(4)?,
        aliases: aliases_from_json(row.get(10)?),
        tmdb_id: row.get(5)?,
        douban_id: row.get(6)?,
        tvdb_id: row.get(7)?,
        bangumi_id: row.get(8)?,
        anilist_id: row.get(9)?,
    })
}

/// The 30-column file-fact projection behind [`map_library_file`] (indices 0..=29).
///
/// Every read that returns a [`LibraryFile`] or a [`FileCoverage`] selects exactly
/// this list (plus `u.season, u.episode` when it joins units), so the file fact has
/// a single mapping site and a new column is added here once.
pub(super) const LIBRARY_FILE_COLUMNS: &str = "f.file_id, f.library_id, f.root_id, f.media_id, f.kind, \
     f.path_key, f.display_path, f.presence_state, f.publication_state, f.file_revision, \
     f.source_version, f.size_bytes, f.stat_known, f.mtime_ns, f.created_ns, f.source_digest, \
     f.source_path, f.transfer_mode, f.resolution, f.codec, f.hdr, f.quality_source, \
     f.confidence, f.filter_score, f.release_quality, f.duration_ms, f.tracks_version, \
     f.poster_path, f.backdrop_path, f.still_path";

pub(super) fn map_library_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<LibraryFile> {
    let kind: String = row.get(4)?;
    let presence: String = row.get(7)?;
    let publication: String = row.get(8)?;
    let stat_known: i64 = row.get(12)?;
    let quality_source: String = row.get(21)?;
    let confidence: String = row.get(22)?;
    let size_bytes: Option<i64> = row.get(11)?;
    Ok(LibraryFile {
        file_id: parse_id(row.get(0)?, 0)?,
        library_id: parse_id(row.get(1)?, 1)?,
        root_id: row.get(2)?,
        media_id: parse_id(row.get(3)?, 3)?,
        kind: kind.parse().unwrap_or(MediaKind::Movie),
        // The read shape exposes the display spelling; `path_key` remains the
        // lookup key used by [`Store::file_coverage_by_path`].
        path: row.get::<_, String>(6)?.into(),
        version: FileVersion {
            revision: row.get(9)?,
            source_version: row.get(10)?,
        },
        presence: FilePresence::from_str(&presence).unwrap_or(FilePresence::Present),
        publication: FilePublication::from_str(&publication).unwrap_or(FilePublication::Published),
        size_bytes: size_bytes.and_then(|size| u64::try_from(size).ok()),
        stat_known: stat_known != 0,
        created_ns: row.get(14)?,
        modified_ns: row.get(13)?,
        source_digest: row.get(15)?,
        source_path: row.get::<_, Option<String>>(16)?.map(PathBuf::from),
        transfer_mode: row.get(17)?,
        resolution: row.get(18)?,
        codec: row.get(19)?,
        hdr: row.get(20)?,
        quality_source: match quality_source.as_str() {
            "probe" => QualitySource::Probe,
            _ => QualitySource::Release,
        },
        confidence: match confidence.as_str() {
            "low" => Confidence::Low,
            _ => Confidence::High,
        },
        filter_score: row.get(23)?,
        release_quality: row.get(24)?,
        duration_ms: row.get(25)?,
        tracks_version: row.get(26)?,
        poster_path: row.get::<_, Option<String>>(27)?.map(PathBuf::from),
        backdrop_path: row.get::<_, Option<String>>(28)?.map(PathBuf::from),
        still_path: row.get::<_, Option<String>>(29)?.map(PathBuf::from),
    })
}

/// A file row plus the unit coordinate selected from the joined `library_file_units`
/// columns (indices 30 and 31). Reads that join units map through here, so the file
/// part of the row is decoded by exactly one function.
pub(super) fn map_file_unit(row: &rusqlite::Row<'_>) -> rusqlite::Result<(LibraryFile, UnitKey)> {
    Ok((map_library_file(row)?, unit_key(row.get(30)?, row.get(31)?)))
}

/// `library_file_units` records the whole-media unit as `(-1, -1)`; a missing join
/// row is the same thing, so both read as [`UnitKey::WHOLE`].
fn unit_key(season: Option<i64>, episode: Option<i64>) -> UnitKey {
    let coordinate = |raw: Option<i64>| {
        raw.and_then(|value| i32::try_from(value).ok())
            .filter(|value| *value >= 0)
    };
    match (coordinate(season), coordinate(episode)) {
        (Some(season), Some(episode)) => UnitKey::new(season, episode),
        _ => UnitKey::WHOLE,
    }
}

/// Fold `(file, unit)` rows into one [`FileCoverage`] per file: first-seen file
/// order is preserved and each file lists its units as `(season ASC, episode ASC)`
/// with duplicates removed, so a file's coverage never depends on join order.
pub(super) fn group_file_coverage(
    rows: impl IntoIterator<Item = (LibraryFile, UnitKey)>,
) -> Vec<FileCoverage> {
    let mut slots: HashMap<LedgerId, usize> = HashMap::new();
    let mut coverage: Vec<FileCoverage> = Vec::new();
    for (file, unit) in rows {
        let slot = match slots.get(&file.file_id) {
            Some(slot) => *slot,
            None => {
                slots.insert(file.file_id, coverage.len());
                coverage.push(FileCoverage {
                    file,
                    units: Vec::new(),
                });
                coverage.len() - 1
            }
        };
        coverage[slot].units.push(unit);
    }
    for file in &mut coverage {
        file.units
            .sort_unstable_by_key(|unit| (unit.season, unit.episode));
        file.units.dedup();
    }
    coverage
}

pub(super) fn map_subscribe(row: &rusqlite::Row<'_>) -> rusqlite::Result<Subscribe> {
    let coverage_kind: String = row.get(3)?;
    let coverage = if coverage_kind == "tv" {
        Coverage::Tv {
            season: row.get::<_, i64>(4)? as u32,
            episode_from: row.get::<_, i64>(5)? as u32,
            episode_to: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
        }
    } else {
        Coverage::Movie
    };
    let fetch_mode: String = row.get(7)?;
    Ok(Subscribe {
        id: parse_id(row.get(0)?, 0)?,
        user_id: parse_id(row.get(1)?, 1)?,
        media_id: parse_id(row.get(2)?, 2)?,
        coverage,
        fetch_mode: parse_token(fetch_mode, 7)?,
        filter_id: parse_id(row.get(8)?, 8)?,
        wash_cut: row.get::<_, i64>(9)? != 0,
        wash_cut_filter_id: row
            .get::<_, Option<String>>(10)?
            .map(|s| parse_id(s, 10))
            .transpose()?,
        full_season_pack: row.get::<_, i64>(11)? != 0,
        downloader_id: row
            .get::<_, Option<String>>(12)?
            .map(|s| parse_id(s, 12))
            .transpose()?,
        tracking_state: row.get::<_, String>(13)?,
        follow_future: row.get::<_, i64>(14)? != 0,
        search_interval_secs: row.get::<_, i64>(15)? as u32,
        keep_old_versions: row.get::<_, i64>(16)? != 0,
        library_id: row
            .get::<_, Option<String>>(17)?
            .map(|s| parse_id(s, 17))
            .transpose()?,
    })
}

pub(super) fn map_site(row: &rusqlite::Row<'_>) -> rusqlite::Result<Site> {
    Ok(Site {
        id: parse_id(row.get(0)?, 0)?,
        name: row.get(1)?,
        url: row.get(2)?,
        profile_id: row.get(3)?,
        cookie: row.get(4)?,
        api_key: row.get(5)?,
        rss_url: row.get(6)?,
        proxy: row.get(7)?,
        rate_limit_per_minute: row.get(8)?,
        cdp_url: row.get(9)?,
        downloader_id: row
            .get::<_, Option<String>>(10)?
            .map(|s| parse_id(s, 10))
            .transpose()?,
        enabled: row.get::<_, i64>(11)? != 0,
        maintenance_via: row
            .get::<_, Option<String>>(12)?
            .as_deref()
            .and_then(|s| s.parse().ok())
            .unwrap_or_default(),
    })
}

/// Serialize filter atoms as a JSON array.
///
/// This replaced the compact DSL (`100:resolution=2160p|50:source=bluray`).
/// Storage is JSON-only: `parse_atoms` refuses the old form rather than decoding
/// it, so every read and write after the cutover speaks the same shape.
pub(super) fn serialize_atoms(atoms: &[FilterAtom]) -> String {
    let values: Vec<serde_json::Value> = atoms
        .iter()
        .map(|atom| {
            let (kind, value) = match &atom.rule {
                domain::AtomRule::Resolution(v) => ("resolution", Some(serde_json::json!(v))),
                domain::AtomRule::Source(v) => ("source", Some(serde_json::json!(v))),
                domain::AtomRule::Codec(v) => ("video_codec", Some(serde_json::json!(v))),
                domain::AtomRule::Free => ("free", None),
                domain::AtomRule::Hr => ("hr", None),
                domain::AtomRule::TitleMatch(v) => ("title", Some(serde_json::json!(v))),
                domain::AtomRule::Hdr(v) => ("hdr", Some(serde_json::json!(v))),
                domain::AtomRule::Size { min_mb, max_mb } => (
                    "size",
                    Some(serde_json::json!({
                        "min_mb": min_mb,
                        "max_mb": max_mb,
                    })),
                ),
                domain::AtomRule::MinSeeders(n) => ("min_seeders", Some(serde_json::json!(n))),
                domain::AtomRule::SubtitleLanguage(v) => ("subtitle", Some(serde_json::json!(v))),
                domain::AtomRule::AudioLanguage(v) => ("audio", Some(serde_json::json!(v))),
                domain::AtomRule::Site(v) => ("site", Some(serde_json::json!(v))),
                domain::AtomRule::WashTarget(v) => ("wash_target", Some(serde_json::json!(v))),
                domain::AtomRule::UpgradeLadder(v) => {
                    ("upgrade_ladder", Some(serde_json::json!(v)))
                }
            };
            let mut obj = serde_json::Map::new();
            obj.insert("priority".into(), serde_json::json!(atom.priority));
            obj.insert("rule".into(), serde_json::json!(kind));
            obj.insert("exclude".into(), serde_json::json!(atom.exclude));
            if let Some(value) = value {
                obj.insert("value".into(), value);
            }
            serde_json::Value::Object(obj)
        })
        .collect();
    serde_json::Value::Array(values).to_string()
}

/// Parse filter atoms from storage.
///
/// Storage only holds the JSON array form written by `serialize_atoms`. Any
/// other shape (the pre-cutover compact DSL included) is rejected instead of
/// being decoded on a best-effort basis, so a stale row surfaces as an error
/// rather than as a silently different rule set.
pub(super) fn parse_atoms(raw: &str) -> Result<Vec<FilterAtom>, StoreError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if !trimmed.starts_with('[') {
        return Err(StoreError::Invalid(
            "filter atoms use a legacy shape; only the JSON array form is read".into(),
        ));
    }
    parse_atoms_json(trimmed)
        .ok_or_else(|| StoreError::Invalid("filter atoms JSON array is malformed".into()))
}

/// Text value of a JSON atom payload; accepts a string or a number.
fn atom_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn parse_atoms_json(raw: &str) -> Option<Vec<FilterAtom>> {
    let serde_json::Value::Array(items) = serde_json::from_str::<serde_json::Value>(raw).ok()?
    else {
        return None;
    };
    Some(
        items
            .into_iter()
            .filter_map(|item| {
                let priority = item["priority"].as_i64()? as i32;
                let kind = item["rule"].as_str()?;
                let exclude = item["exclude"].as_bool().unwrap_or(false);
                let value = item.get("value").unwrap_or(&serde_json::Value::Null);
                let text = atom_text(value);
                let rule = match kind {
                    "resolution" => domain::AtomRule::Resolution(text),
                    "source" => domain::AtomRule::Source(text),
                    "video_codec" => domain::AtomRule::Codec(text),
                    "free" => domain::AtomRule::Free,
                    "hr" => domain::AtomRule::Hr,
                    "title" => domain::AtomRule::TitleMatch(text),
                    "hdr" => domain::AtomRule::Hdr(text),
                    "size" => domain::AtomRule::Size {
                        min_mb: value["min_mb"].as_u64().filter(|m| *m > 0),
                        max_mb: value["max_mb"].as_u64().filter(|m| *m > 0),
                    },
                    "min_seeders" => domain::AtomRule::MinSeeders(text.parse().ok()?),
                    "subtitle" => domain::AtomRule::SubtitleLanguage(text),
                    "audio" => domain::AtomRule::AudioLanguage(text),
                    "site" => domain::AtomRule::Site(text),
                    "wash_target" => domain::AtomRule::WashTarget(text),
                    "upgrade_ladder" => domain::AtomRule::UpgradeLadder(text),
                    _ => return None,
                };
                Some(FilterAtom {
                    priority,
                    rule,
                    exclude,
                })
            })
            .collect(),
    )
}

pub(super) fn torrent_from_json(value: &serde_json::Value) -> Result<Torrent, StoreError> {
    let site_id = value
        .get("site_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| StoreError::FetchMode("pending torrent site_id".into()))?;
    Ok(Torrent {
        site_id: SiteId::from_str(site_id)?,
        title: value
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        enclosure: value
            .get("enclosure")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        size_bytes: value.get("size_bytes").and_then(|v| v.as_u64()),
        seeders: value
            .get("seeders")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        free: value.get("free").and_then(|v| v.as_bool()).unwrap_or(false),
        hr: value.get("hr").and_then(|v| v.as_bool()).unwrap_or(false),
        imdb_id: value
            .get("imdb_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        id: value.get("id").and_then(|v| v.as_str()).map(str::to_string),
        leechers: value
            .get("leechers")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        snatched: value
            .get("snatched")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        upload_time: value
            .get("upload_time")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        detail_url: value
            .get("detail_url")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        category: value
            .get("category")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        poster_url: value
            .get("poster_url")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}
