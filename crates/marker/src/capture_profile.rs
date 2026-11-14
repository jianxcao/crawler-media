//! Capture identity shared by producers and persisted-evidence consumers.
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FingerprintCaptureProfile {
    pub algorithm_version: u32,
    pub preset: String,
    pub pcm_sample_rate: u32,
    pub pcm_channels: u32,
    pub pcm_format: String,
    pub audio_stream_index: Option<u32>,
    pub audio_selection_version: u32,
    pub time_mapping_version: u32,
}

impl Default for FingerprintCaptureProfile {
    fn default() -> Self {
        Self {
            algorithm_version: crate::FINGERPRINT_ALGORITHM_VERSION,
            preset: "chromaprint_default".into(),
            pcm_sample_rate: 16000,
            pcm_channels: 1,
            pcm_format: "s16le".into(),
            audio_stream_index: None,
            audio_selection_version: 2,
            time_mapping_version: 3,
        }
    }
}

pub fn capture_profile_key(profile: &FingerprintCaptureProfile) -> String {
    let mut digest = Sha256::new();
    digest.update(b"crawler-media-capture-profile-v2\0");
    digest.update(profile.algorithm_version.to_le_bytes());
    digest.update(profile.preset.as_bytes());
    digest.update(profile.pcm_sample_rate.to_le_bytes());
    digest.update(profile.pcm_channels.to_le_bytes());
    digest.update(profile.pcm_format.as_bytes());
    digest.update(profile.audio_stream_index.unwrap_or(u32::MAX).to_le_bytes());
    digest.update(profile.audio_selection_version.to_le_bytes());
    digest.update(profile.time_mapping_version.to_le_bytes());
    format!("{:x}", digest.finalize())
}

pub fn profile_for_source(source: Option<&crate::CaptureSourceFacts>) -> FingerprintCaptureProfile {
    FingerprintCaptureProfile {
        audio_stream_index: source.map(|source| source.audio_stream_index),
        time_mapping_version: if source.is_some_and(quantized_aac) {
            4
        } else {
            3
        },
        ..Default::default()
    }
}

pub(crate) fn quantized_aac(source: &crate::CaptureSourceFacts) -> bool {
    source.codec_name.as_deref() == Some("aac")
        && matches!(source.native_sample_rate, Some(44_100 | 48_000))
        && source.audio_channels == Some(2)
        && source.stream_time_base.as_deref() == Some("1/1000")
}
