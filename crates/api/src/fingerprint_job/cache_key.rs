use std::path::Path;

use sha2::{Digest, Sha256};

/// Content token of `path`: the one shared encoder, [`library::current_source_version`].
pub fn media_source_version(path: &Path) -> String {
    library::current_source_version(path)
}

pub fn fingerprint_cache_key(
    source_version: &str,
    sample_duration_secs: u32,
    media_duration_ms: Option<i64>,
) -> String {
    fingerprint_cache_key_with_profile(
        source_version,
        sample_duration_secs,
        media_duration_ms,
        &FingerprintCaptureProfile::default(),
    )
}

pub fn fingerprint_cache_key_with_profile(
    source_version: &str,
    sample_duration_secs: u32,
    _media_duration_ms: Option<i64>,
    profile: &FingerprintCaptureProfile,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"crawler-media-fingerprint-profile-v3\0");
    digest.update(capture_profile_key(profile).as_bytes());
    digest.update(source_version.as_bytes());
    digest.update(marker::FINGERPRINT_ALGORITHM_VERSION.to_le_bytes());
    digest.update(sample_duration_secs.to_le_bytes());
    digest_hex(digest.finalize())
}

pub use marker::capture_profile::{FingerprintCaptureProfile, capture_profile_key};

pub fn analysis_policy_key(policy: &marker::adaptive::SamplingPolicy) -> String {
    let mut digest = Sha256::new();
    digest.update(b"crawler-media-analysis-policy-v1\0");
    digest.update(marker::boundary::EVIDENCE_POLICY_VERSION.to_le_bytes());
    digest.update(policy.boundary.pcm_frame_ms.to_le_bytes());
    digest.update(policy.boundary.silence_dbfs.to_le_bytes());
    digest.update(policy.boundary.silence_duration_ms.to_le_bytes());
    digest.update(policy.boundary.transition_db.to_le_bytes());
    digest.update(policy.boundary.transition_window_ms.to_le_bytes());
    digest.update(policy.boundary.context_ms.to_le_bytes());
    digest.update(policy.boundary.agreement_ms.to_le_bytes());
    digest.update(policy.boundary.min_guard_ms.to_le_bytes());
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
        assert_eq!(
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
