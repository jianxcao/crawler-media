use domain::{Confidence, FileCoverage, Release};
use rusqlite::{OptionalExtension, params};
use subscribe::SubscribeFacts;

use super::{Store, StoreError};

/// The coordinates a legacy per-unit row carried; a whole-file coverage has none.
///
/// A coverage may hold several units（同一物理文件覆盖多集）。Legacy 调用方拿到的是
/// join 出来的逐 unit 行，这里取升序首个 unit 作为 Release 的坐标；已记入收窄清单。
fn coverage_coordinates(coverage: &FileCoverage) -> (Option<u32>, Option<u32>) {
    match coverage.units.first() {
        Some(unit) if !unit.is_whole() => (
            u32::try_from(unit.season).ok(),
            u32::try_from(unit.episode).ok(),
        ),
        _ => (None, None),
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredQuality {
    resolution: Option<String>,
    source: Option<String>,
    codec: Option<String>,
    hdr: Option<String>,
}

impl From<&Release> for StoredQuality {
    fn from(release: &Release) -> Self {
        Self {
            resolution: release.resolution.clone(),
            source: release.source.clone(),
            codec: release.codec.clone(),
            hdr: release.hdr.clone(),
        }
    }
}

impl Store {
    /// Persist provenance dimensions that stream probing cannot recover.
    /// The Library row owns this quality, so a later Subscribe can reuse it.
    pub(crate) fn persist_owned_quality(&self, facts: &SubscribeFacts) -> Result<(), StoreError> {
        let tx = self.library.unchecked_transaction()?;
        for (_, fact) in facts.entries() {
            let Some(path) = fact.path.as_deref() else {
                continue;
            };
            let Some(quality) = facts.quality(path) else {
                continue;
            };
            tx.execute(
                "UPDATE library_files SET release_quality = ?2 WHERE path_key = norm_path_key(?1) OR display_path = ?1",
                params![path, serde_json::to_string(&StoredQuality::from(quality))?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Load stable Release facts and overlay the latest stream probe fields.
    pub(crate) fn owned_quality(&self, coverage: &FileCoverage) -> Result<Release, StoreError> {
        let (season, episode) = coverage_coordinates(coverage);
        let path = coverage.file.path.to_string_lossy().into_owned();
        let json: Option<String> = self.library.query_row(
            "SELECT release_quality FROM library_files WHERE path_key = norm_path_key(?1) OR display_path = ?1",
            [path.as_str()],
            |r| r.get(0),
        ).optional()?.flatten();
        let mut quality = match json {
            Some(json) => {
                let stored: StoredQuality = serde_json::from_str(&json)?;
                Release {
                    title: String::new(),
                    year: None,
                    season,
                    episode,
                    episode_to: None,
                    resolution: stored.resolution,
                    source: stored.source,
                    codec: stored.codec,
                    hdr: stored.hdr,
                    subtitle_language: None,
                    audio_language: None,
                    group: None,
                    confidence: coverage.file.confidence,
                    season_inferred: false,
                }
            }
            None => Release {
                title: String::new(),
                year: None,
                season,
                episode,
                episode_to: None,
                resolution: None,
                source: None,
                codec: None,
                hdr: None,
                subtitle_language: None,
                audio_language: None,
                group: None,
                confidence: Confidence::High,
                season_inferred: false,
            },
        };
        quality.season = season;
        quality.episode = episode;
        quality.resolution = coverage.file.resolution.clone().or(quality.resolution);
        quality.codec = coverage.file.codec.clone().or(quality.codec);
        quality.hdr = coverage.file.hdr.clone().or(quality.hdr);
        quality.confidence = coverage.file.confidence;
        Ok(quality)
    }
}
