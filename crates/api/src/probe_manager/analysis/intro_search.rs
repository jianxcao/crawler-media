//! Finite independent captures; no PCM or timeline is joined across windows.
use marker::{SampleWindow, SegmentKind};
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(in crate::probe_manager) struct KindSearch {
    pub kind: SegmentKind,
    pub windows: Vec<SampleWindow>,
    pub pending: Option<SampleWindow>,
    pub terminal_reason: Option<String>,
}
impl KindSearch {
    pub fn new(kind: SegmentKind, window: SampleWindow) -> Self {
        Self {
            kind,
            windows: vec![],
            pending: Some(window),
            terminal_reason: None,
        }
    }
    pub fn record(&mut self, window: SampleWindow) {
        if !self.windows.contains(&window) {
            self.windows.push(window);
        }
    }
    pub fn next(&self, duration: Option<i64>, max_windows: usize) -> Option<SampleWindow> {
        if self.pending.is_some() || self.windows.len() >= max_windows {
            return None;
        }
        let bound = duration.filter(|d| *d > 0).unwrap_or(600_000).min(600_000);
        [(0, 180_000), (120_000, 420_000), (300_000, 600_000)]
            .into_iter()
            .map(|(start, end)| SampleWindow::new(start, end.min(bound)))
            .find(|window| {
                window.end_ms > window.start_ms
                    && !self
                        .windows
                        .iter()
                        .any(|old| old.start_ms <= window.start_ms && old.end_ms >= window.end_ms)
            })
    }
}
pub(super) fn name(kind: SegmentKind) -> &'static str {
    match kind {
        SegmentKind::Intro => "intro",
        SegmentKind::Outro => "outro",
    }
}
pub(super) fn eligible(evidence: &marker::EpisodeEvidence) -> bool {
    let capture = &evidence.capture;
    matches!(capture.metrics.time_mapping_version, 3 | 4)
        && capture.metrics.pcm_timeline == marker::PcmTimeline::Contiguous
        && capture.metrics.source_facts.is_some()
        && marker::boundary::evidence::candidate_pcm_coverage(capture).is_some()
        && if capture.metrics.time_mapping_version == 3 {
            capture.metrics.contiguous_pcm_start_ms == Some(capture.window.start_ms)
                && marker::boundary::evidence::complete_pcm(capture)
        } else {
            marker::boundary::evidence::capture_origin_covers_request_start(capture)
                && capture
                    .pcm_duration_ms
                    .is_some_and(|duration| duration >= capture.window.duration_ms())
        }
}
