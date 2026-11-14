use super::capture_profile_key;
pub use library::capture_source;
pub use marker::capture_profile::profile_for_source;

pub(crate) fn source_for_file(
    store: &crate::Store,
    file: &domain::LibraryFile,
) -> Option<marker::CaptureSourceFacts> {
    source_for_file_selected(store, file, None)
}

pub(crate) fn source_for_file_selected(
    store: &crate::Store,
    file: &domain::LibraryFile,
    selected: Option<u32>,
) -> Option<marker::CaptureSourceFacts> {
    let id = file.file_id.to_string();
    let version = store.get_media_info_cache_version(&id).ok()??;
    if version.source_version != file.version.source_version {
        return None;
    }
    capture_source(&store.get_file_meta(&id).ok()??, selected).ok()
}

pub(crate) fn key_for_file(
    store: &crate::Store,
    file: &domain::LibraryFile,
    seconds: u32,
    duration: Option<i64>,
) -> String {
    super::fingerprint_cache_key_with_profile(
        &file.version.source_version,
        seconds,
        duration,
        &profile_for_source(source_for_file(store, file).as_ref()),
    )
}

pub(crate) fn reusable_capture(
    capture: &marker::CapturedFingerprint,
    profile_key: &str,
    window: &marker::SampleWindow,
    expected_source: Option<&marker::CaptureSourceFacts>,
) -> bool {
    let source = capture.metrics.source_facts.as_ref();
    capture.window == *window
        && !capture.words.is_empty()
        && source.is_some()
        && source == expected_source
        && capture.metrics.audio_stream_index == source.map(|s| s.audio_stream_index)
        && capture_profile_key(&profile_for_source(source)) == profile_key
        && capture.metrics.time_mapping_version == profile_for_source(source).time_mapping_version
        && capture.metrics.pcm_timeline == marker::PcmTimeline::Contiguous
        && if capture.metrics.time_mapping_version == 4 {
            marker::boundary::evidence::candidate_pcm_coverage(capture).is_some()
                && capture
                    .metrics
                    .quantized_mapping
                    .as_ref()
                    .is_some_and(|mapping| {
                        let requested_us = i128::from(capture.window.start_ms) * 1000;
                        i128::from(mapping.origin_min_us) <= requested_us
                            && i128::from(mapping.origin_max_us) >= requested_us
                    })
        } else {
            capture.metrics.contiguous_pcm_start_ms == Some(capture.window.start_ms)
        }
}
