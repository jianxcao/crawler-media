//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub(crate) fn persist_owned_quality(&self, facts: &SubscribeFacts) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        for (_, fact) in facts.entries() {
            let Some(path) = fact.path.as_deref() else {
                continue;
            };
            let Some(quality) = facts.quality(path) else {
                continue;
            };
            tx.execute(
                "UPDATE library_files SET release_quality = ?2 WHERE path_key = norm_path_key(?1)",
                params![path, serde_json::to_string(&StoredQuality::from(quality))?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn owned_quality(&self, coverage: &FileCoverage) -> Result<Release, StoreError> {
        let (season, episode) = coverage_coordinates(coverage);
        let path = coverage.file.path.to_string_lossy().into_owned();
        let stored: Option<(Option<String>, bool)> = self
            .library
            .query_row(
                "SELECT release_quality, hdr_observed FROM library_files WHERE path_key = norm_path_key(?1)",
                [path.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (json, hdr_observed) = stored.unwrap_or((None, false));
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
        quality.hdr = if hdr_observed {
            coverage.file.hdr.clone()
        } else {
            coverage.file.hdr.clone().or(quality.hdr)
        };
        quality.confidence = coverage.file.confidence;
        Ok(quality)
    }
}
