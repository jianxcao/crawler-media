//! Select the versioned audio header used by capture and model validation.
pub fn capture_source(
    tracks: &crate::Tracks,
    selected: Option<u32>,
) -> Result<marker::CaptureSourceFacts, String> {
    let track = tracks
        .audio
        .iter()
        .filter(|track| selected.is_none_or(|index| track.stream_index == Some(index)))
        .min_by_key(|track| (!track.is_default, track.stream_index.unwrap_or(u32::MAX)))
        .ok_or("requested audio stream unavailable")?;
    let facts = track
        .capture_source
        .clone()
        .ok_or("audio source timeline facts unavailable")?;
    if track.stream_index != Some(facts.audio_stream_index) {
        return Err("audio stream/header identity mismatch".into());
    }
    Ok(facts)
}
