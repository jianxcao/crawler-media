use std::path::Path;
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

pub fn media_source_version(path: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(b"crawler-media-source-v1\0");
    digest.update(path.to_string_lossy().as_bytes());
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("strm"))
        && let Ok(contents) = std::fs::read(path)
    {
        // strm 没有本地媒体字节。URL 文本不变就视为同一文件，
        // 同步或目录操作改掉 mtime/inode 不能让已有媒体信息和声纹失效。
        digest.update(b"\0strm-target\0");
        digest.update(contents);
        return digest_hex(digest.finalize());
    }
    if let Ok(metadata) = std::fs::metadata(path) {
        digest.update(metadata.len().to_le_bytes());
        if let Ok(modified) = metadata.modified()
            && let Ok(age) = modified.duration_since(UNIX_EPOCH)
        {
            digest.update(age.as_nanos().to_le_bytes());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            digest.update(metadata.dev().to_le_bytes());
            digest.update(metadata.ino().to_le_bytes());
        }
    }
    digest_hex(digest.finalize())
}

pub fn fingerprint_cache_key(
    source_version: &str,
    sample_duration_secs: u32,
    media_duration_ms: Option<i64>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"crawler-media-fingerprint-profile-v1\0");
    digest.update(source_version.as_bytes());
    digest.update(marker::FINGERPRINT_ALGORITHM_VERSION.to_le_bytes());
    digest.update(sample_duration_secs.to_le_bytes());
    digest.update(media_duration_ms.unwrap_or_default().to_le_bytes());
    digest.update(marker::MIN_MATCH_DURATION_SECS.to_le_bytes());
    digest.update(marker::MAX_MATCH_DURATION_SECS.to_le_bytes());
    digest_hex(digest.finalize())
}

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
            algorithm_version: marker::FINGERPRINT_ALGORITHM_VERSION,
            preset: "chromaprint_default".into(),
            pcm_sample_rate: 16000,
            pcm_channels: 1,
            pcm_format: "s16le".into(),
            audio_stream_index: None,
            audio_selection_version: 1,
            time_mapping_version: 1,
        }
    }
}

pub fn capture_profile_key(profile: &FingerprintCaptureProfile) -> String {
    let mut digest = Sha256::new();
    digest.update(b"crawler-media-capture-profile-v1\0");
    digest.update(profile.algorithm_version.to_le_bytes());
    digest.update(profile.preset.as_bytes());
    digest.update(profile.pcm_sample_rate.to_le_bytes());
    digest.update(profile.pcm_channels.to_le_bytes());
    digest.update(profile.pcm_format.as_bytes());
    digest.update(profile.audio_stream_index.unwrap_or(u32::MAX).to_le_bytes());
    digest.update(profile.audio_selection_version.to_le_bytes());
    digest.update(profile.time_mapping_version.to_le_bytes());
    digest_hex(digest.finalize())
}

pub fn analysis_policy_key(policy: &marker::adaptive::SamplingPolicy) -> String {
    let mut digest = Sha256::new();
    digest.update(b"crawler-media-analysis-policy-v1\0");
    digest.update(policy.seed_count.to_le_bytes());
    digest.update(policy.max_seed_count.to_le_bytes());
    digest.update(policy.max_templates_per_kind.to_le_bytes());
    digest.update(policy.context_margin_ms.to_le_bytes());
    digest.update(policy.min_window_saving_ratio.to_le_bytes());
    digest.update(policy.min_match_duration_ms.to_le_bytes());
    digest.update(policy.max_match_duration_ms.to_le_bytes());
    digest.update(policy.max_score.to_le_bytes());
    digest.update(policy.min_reference_coverage.to_le_bytes());
    digest.update(policy.max_internal_gap_ms.to_le_bytes());
    digest.update(policy.max_reference_boundary_delta_ms.to_le_bytes());
    digest.update(policy.min_guard_evidence_ms.to_le_bytes());
    digest.update(policy.template_edge_anchor_ms.to_le_bytes());
    digest.update(policy.max_windows_per_kind.to_le_bytes());
    digest.update(policy.max_attempts_per_kind.to_le_bytes());
    digest.update(policy.process_deadline_ms.to_le_bytes());
    digest.update(policy.full_window_duration_secs.to_le_bytes());
    digest_hex(digest.finalize())
}

fn digest_hex(digest: impl AsRef<[u8]>) -> String {
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{fingerprint_cache_key, media_source_version};
    use std::path::Path;

    fn set_mtime_later(path: &Path) {
        let touched =
            path.metadata().unwrap().modified().unwrap() + std::time::Duration::from_secs(5);
        let secs = touched
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let bytes = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        let mut times = [secs, 0, secs, 0];
        let result = unsafe { libc_utimes(bytes.as_ptr(), times.as_mut_ptr()) };
        assert_eq!(result, 0, "设置测试文件修改时间失败");
    }

    unsafe extern "C" {
        #[link_name = "utimes"]
        fn libc_utimes(path: *const i8, times: *mut i64) -> i32;
    }

    #[test]
    fn fingerprint_cache_key_changes_with_profile_duration_and_strm_target() {
        let temp = tempfile::tempdir().unwrap();
        let stream = temp.path().join("episode.strm");
        std::fs::write(&stream, "https://cdn.example/episode-a.mkv?token=hidden\n").unwrap();
        let first_source = media_source_version(&stream);
        let first = fingerprint_cache_key(&first_source, 180, Some(2_700_000));

        assert_ne!(
            first,
            fingerprint_cache_key(&first_source, 181, Some(2_700_000))
        );
        assert_ne!(
            first,
            fingerprint_cache_key(&first_source, 180, Some(2_701_000))
        );
        std::fs::write(&stream, "https://cdn.example/episode-a.mkv?token=hidden\n").unwrap();
        set_mtime_later(&stream);
        assert_eq!(
            first_source,
            media_source_version(&stream),
            "strm URL 未变时，修改时间不能让缓存版本变化"
        );

        std::fs::write(&stream, "https://cdn.example/episode-b.mkv?token=hidden\n").unwrap();
        let second_source = media_source_version(&stream);
        assert_ne!(first_source, second_source);
        assert_ne!(
            first,
            fingerprint_cache_key(&second_source, 180, Some(2_700_000))
        );
        assert!(!second_source.contains("hidden"));
        assert_ne!(
            media_source_version(Path::new("https://cdn.example/episode.mkv")),
            ""
        );

        let video = temp.path().join("episode.mp4");
        std::fs::write(&video, b"video-bytes").unwrap();
        let video_source = media_source_version(&video);
        set_mtime_later(&video);
        assert_ne!(
            video_source,
            media_source_version(&video),
            "真实视频文件仍按修改时间判断内容是否变化"
        );
    }
}
