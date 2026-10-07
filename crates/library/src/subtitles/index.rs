use crate::Tracks;

/// 为字幕轨计算稳定的 stream index。
/// 内封轨保留已有的 stream_index。
/// 外挂轨（无 stream_index 或 is_external 为 true）分配高于所有已知视频/音频/内封字幕 stream_index 的稳定序号。
pub fn subtitle_index(tracks: &Tracks, ordinal: usize) -> u32 {
    let sub = tracks.subtitles.get(ordinal);
    if let Some(sub) = sub {
        if !sub.is_external && sub.stream_index.is_some() {
            return sub.stream_index.unwrap();
        }
    }
    let max_stream_idx = tracks
        .video
        .as_ref()
        .and_then(|v| v.stream_index)
        .into_iter()
        .chain(tracks.audio.iter().filter_map(|a| a.stream_index))
        .chain(
            tracks
                .subtitles
                .iter()
                .filter(|s| !s.is_external)
                .filter_map(|s| s.stream_index),
        )
        .max()
        .unwrap_or(0);

    let base = max_stream_idx + 1;
    // 统计 ordinal 之前有多少个外挂轨
    let external_before = tracks
        .subtitles
        .iter()
        .take(ordinal)
        .filter(|s| s.is_external || s.stream_index.is_none())
        .count() as u32;

    base + external_before
}

/// 根据请求的 index 在 tracks.subtitles 中定位对应的字幕轨
pub fn find_subtitle_by_index<'a>(tracks: &'a Tracks, index: u32) -> Option<&'a crate::SubtitleTrack> {
    for (i, sub) in tracks.subtitles.iter().enumerate() {
        if subtitle_index(tracks, i) == index {
            return Some(sub);
        }
    }
    None
}
