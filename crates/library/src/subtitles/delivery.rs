use crate::Tracks;
use super::conversion::srt_to_vtt;
use super::index::find_subtitle_by_index;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitlePayload {
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
}

/// 共享字幕交付能力：根据请求的 format，查找并转换/交付字幕
pub fn deliver_subtitle(
    tracks: &Tracks,
    index: u32,
    wants_vtt: bool,
) -> Result<SubtitlePayload, DeliveryError> {
    let sub = find_subtitle_by_index(tracks, index).ok_or(DeliveryError::TrackNotFound)?;
    let Some(path) = &sub.path else {
        return Err(DeliveryError::FileMissing);
    };
    let bytes = std::fs::read(path).map_err(|e| DeliveryError::Io(e.to_string()))?;
    let codec = sub.codec.as_deref().unwrap_or("srt").to_lowercase();
    if codec == "srt" && wants_vtt {
        let srt_text = String::from_utf8_lossy(&bytes);
        let vtt_text = srt_to_vtt(&srt_text);
        return Ok(SubtitlePayload {
            content_type: "text/vtt; charset=utf-8",
            bytes: vtt_text.into_bytes(),
        });
    }
    let content_type = match codec.as_str() {
        "vtt" => "text/vtt; charset=utf-8",
        "ass" | "ssa" => "text/x-ssa; charset=utf-8",
        _ => "text/plain; charset=utf-8",
    };
    Ok(SubtitlePayload {
        content_type,
        bytes,
    })
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DeliveryError {
    #[error("track not found")]
    TrackNotFound,
    #[error("file missing")]
    FileMissing,
    #[error("io error: {0}")]
    Io(String),
}
