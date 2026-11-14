use crate::EpisodeEvidence;
use sha2::{Digest, Sha256};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Endpoint {
    file: String,
    source: String,
    profile: String,
    kind: u8,
    start_ms: i64,
    end_ms: i64,
    audio_stream: Option<u32>,
    mapping_version: u32,
    words_len: usize,
    words_digest: [u8; 32],
}

impl Endpoint {
    fn new(e: &EpisodeEvidence) -> Self {
        let mut digest = Sha256::new();
        for word in &e.capture.words {
            digest.update(word.to_le_bytes());
        }
        Self {
            file: e.ledger_id.clone(),
            source: e.source_version.clone(),
            profile: e.capture_profile_key.clone(),
            kind: match e.kind {
                crate::SegmentKind::Intro => 0,
                crate::SegmentKind::Outro => 1,
            },
            start_ms: e.capture.window.start_ms,
            end_ms: e.capture.window.end_ms,
            audio_stream: e.capture.metrics.audio_stream_index,
            mapping_version: e.capture.metrics.time_mapping_version,
            words_len: e.capture.words.len(),
            words_digest: digest.finalize().into(),
        }
    }

    fn allocated_bytes(&self) -> usize {
        self.file
            .capacity()
            .saturating_add(self.source.capacity())
            .saturating_add(self.profile.capacity())
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct PairKey {
    first: Endpoint,
    second: Endpoint,
    policy: String,
    min: u32,
    max: u32,
    algorithm_version: u32,
}

impl PairKey {
    pub(super) fn new(
        first: &EpisodeEvidence,
        second: &EpisodeEvidence,
        policy: &str,
        min: f32,
        max: f32,
    ) -> Self {
        Self {
            first: Endpoint::new(first),
            second: Endpoint::new(second),
            policy: policy.into(),
            min: min.to_bits(),
            max: max.to_bits(),
            algorithm_version: super::RAW_COMPARISON_ALGORITHM_VERSION,
        }
    }

    pub(super) fn allocated_bytes(&self) -> usize {
        self.first
            .allocated_bytes()
            .saturating_add(self.second.allocated_bytes())
            .saturating_add(self.policy.capacity())
    }
}
