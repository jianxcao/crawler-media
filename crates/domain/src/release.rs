use crate::{Confidence, Coverage};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub title: String,
    pub year: Option<u16>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub episode_to: Option<u32>,
    pub resolution: Option<String>,
    pub source: Option<String>,
    pub codec: Option<String>,
    pub hdr: Option<String>,
    pub subtitle_language: Option<String>,
    pub audio_language: Option<String>,
    pub group: Option<String>,
    pub confidence: Confidence,
    /// G-H2: the title carried no season marker; `season` is the parser's S01 default.
    pub season_inferred: bool,
}

impl Release {
    /// Upper bound of episodes one video file can plausibly carry. Larger
    /// parsed ranges are almost always mis-parses (e.g. a resolution read as
    /// the range end) and must be surfaced, not silently fanned out.
    pub const MAX_PLAUSIBLE_FILE_EPISODES: u32 = 200;

    /// Raw parsed `(from, to)` when it spans more episodes than one file can
    /// plausibly hold; `None` for single episodes and normal ranges.
    pub fn abnormal_episode_span(&self) -> Option<(u32, u32)> {
        let from = self.episode?;
        let to = self.episode_to?;
        (to >= from && to - from + 1 > Self::MAX_PLAUSIBLE_FILE_EPISODES).then_some((from, to))
    }

    /// Inclusive episode span after clamping inverted / oversized ranges.
    /// Callers must not iterate `episode..=episode_to` themselves.
    pub fn episode_span(&self) -> Option<(u32, u32)> {
        let from = self.episode?;
        let to = self.episode_to.unwrap_or(from).max(from);
        let capped = from.saturating_add(Coverage::MAX_EPISODES.saturating_sub(1));
        Some((from, to.min(capped)))
    }

    /// Bounded TV units this Release covers. Movie / unknown season stays empty.
    pub fn covered_episodes(&self) -> Vec<(u32, u32)> {
        let Some(season) = self.season else {
            return Vec::new();
        };
        let Some((from, to)) = self.episode_span() else {
            return Vec::new();
        };
        (from..=to).map(|episode| (season, episode)).collect()
    }

    /// Episode count used for size amortization and similar per-episode math.
    pub fn episode_count(&self) -> u32 {
        self.episode_span()
            .map(|(from, to)| to.saturating_sub(from) + 1)
            .unwrap_or(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tv(from: u32, to: Option<u32>) -> Release {
        Release {
            season: Some(1),
            episode: Some(from),
            episode_to: to,
            title: "x".into(),
            year: None,
            resolution: None,
            source: None,
            codec: None,
            hdr: None,
            subtitle_language: None,
            audio_language: None,
            group: None,
            confidence: Confidence::High,
            season_inferred: false,
        }
    }

    #[test]
    fn abnormal_episode_span_is_flagged() {
        assert_eq!(tv(90, None).abnormal_episode_span(), None);
        assert_eq!(tv(1, Some(24)).abnormal_episode_span(), None);
        assert_eq!(
            tv(1, Some(Release::MAX_PLAUSIBLE_FILE_EPISODES)).abnormal_episode_span(),
            None
        );
        assert_eq!(
            tv(1, Some(Release::MAX_PLAUSIBLE_FILE_EPISODES + 1)).abnormal_episode_span(),
            Some((1, Release::MAX_PLAUSIBLE_FILE_EPISODES + 1))
        );
        // The 仙逆 shape: E90 with a resolution read as the range end.
        assert_eq!(tv(90, Some(2160)).abnormal_episode_span(), Some((90, 2160)));
    }

    fn tv_range(from: u32, to: u32) -> Release {
        Release {
            title: "Show".into(),
            year: None,
            season: Some(1),
            episode: Some(from),
            episode_to: Some(to),
            resolution: None,
            source: None,
            codec: None,
            hdr: None,
            subtitle_language: None,
            audio_language: None,
            group: None,
            confidence: Confidence::High,
            season_inferred: false,
        }
    }

    #[test]
    fn huge_release_range_is_capped_without_enumerating_u32_max() {
        let release = tv_range(1, u32::MAX);
        let (from, to) = release.episode_span().expect("range");
        assert_eq!(from, 1);
        assert_eq!(to, Coverage::MAX_EPISODES);
        let units = release.covered_episodes();
        assert_eq!(units.len(), Coverage::MAX_EPISODES as usize);
        assert_eq!(units[0], (1, 1));
        assert_eq!(units[units.len() - 1], (1, Coverage::MAX_EPISODES));
        assert_eq!(release.episode_count(), Coverage::MAX_EPISODES);
    }
}
