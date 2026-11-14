use super::*;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, process::Command};

pub(super) fn build() -> BuildProvenance {
    BuildProvenance {
        profile: env!("CM_BUILD_PROFILE").into(),
        opt_level: env!("CM_BUILD_OPT_LEVEL").into(),
        debug: env!("CM_BUILD_DEBUG").into(),
        target: env!("CM_BUILD_TARGET").into(),
        encoded_rustflags: env!("CM_BUILD_RUSTFLAGS").into(),
        dependency_lock_sha256: format!(
            "sha256:{:x}",
            Sha256::digest(include_bytes!("../../../../../Cargo.lock"))
        ),
        rustc: version("rustc"),
        ffmpeg: version("ffmpeg"),
        ffprobe: version("ffprobe"),
        effective_settings: BTreeMap::from([
            (
                "capture_time_mapping_version".into(),
                "fine_clock:3,aac_packet_clock:4".into(),
            ),
            ("audio_selection_version".into(), "2".into()),
            (
                "boundary_evidence_policy_version".into(),
                marker::boundary::EVIDENCE_POLICY_VERSION.to_string(),
            ),
        ]),
    }
}
fn version(program: &str) -> String {
    let output = Command::new(program)
        .arg(if program == "rustc" {
            "--version"
        } else {
            "-version"
        })
        .output();
    match output {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .take(5)
            .collect::<Vec<_>>()
            .join("\n"),
        _ => "unavailable".into(),
    }
}
pub(super) fn config_hash(
    options: &BenchmarkOptions,
    build: &BuildProvenance,
) -> Result<String, String> {
    let config = serde_json::json!({"mode":options.mode,"capture_policy":options.capture_policy,"build":build,
        "sampling_policy":crate::fingerprint_job::analysis_policy_key(&marker::SamplingPolicy::default()),
        "fingerprint_duration_secs":180,"protocol":"production_enqueue_to_probe_dispatch_release_and_validated_frame_terminal_v1"});
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&config).map_err(|e| e.to_string())?)
    ))
}
