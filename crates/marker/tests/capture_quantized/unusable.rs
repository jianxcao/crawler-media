use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn untrusted_probe_output_is_bounded_and_never_promoted_to_pcm_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let payloads = container::payloads(dir.path());
    let file = dir.path().join("source.mka");
    container::write(&file, &payloads, false, 0, |_| 0);
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_packets",
            "-show_frames",
            "-show_streams",
            "-show_format",
            "-show_data",
            "-of",
            "json",
        ])
        .arg(&file)
        .output()
        .unwrap();
    assert!(output.status.success());
    let mut proof: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    proof["packets_and_frames"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|item| item["type"] == "frame")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("pkt_pos");
    let missing = dir.path().join("missing.json");
    std::fs::write(&missing, serde_json::to_vec(&proof).unwrap()).unwrap();
    let fake = dir.path().join("ffprobe");
    std::fs::write(&fake, "#!/usr/bin/env python3\nimport os,sys\ncase=os.environ['CM_PROOF_CASE']\nif case=='capacity':sys.stdout.buffer.write(b' '*(32*1024*1024+1))\nelif case=='malformed':sys.stdout.write('{broken')\nelse:sys.stdout.buffer.write(open(os.environ['CM_PROOF_FILE'],'rb').read())\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap()
    );
    for case in ["missing", "malformed", "capacity", "overflow", "cancel"] {
        let result = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "unusable::probe_child", "--nocapture"])
            .env("PATH", &path)
            .env("CM_PROOF_CASE", case)
            .env("CM_PROOF_FILE", &missing)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{case}: {}",
            String::from_utf8_lossy(&result.stdout)
        );
    }
}

#[test]
fn probe_child() {
    let Ok(case) = std::env::var("CM_PROOF_CASE") else {
        return;
    };
    let source = marker::CaptureSourceFacts {
        audio_stream_index: 0,
        format_start_time_us: Some(0),
        stream_start_pts: Some(0),
        stream_time_base: Some("1/1000".into()),
        codec_name: Some("aac".into()),
        native_sample_rate: Some(44_100),
        audio_channels: Some(2),
        provenance: "injected_probe_header".into(),
    };
    let control = marker::WorkControl::default();
    if case == "cancel" {
        control.cancel();
    }
    let request = CaptureRequest {
        path: "/tmp/owned-probe-observation".into(),
        window: SampleWindow::new(0, if case == "overflow" { i64::MAX } else { 10_000 }),
        audio_stream_index: Some(0),
        process_deadline_ms: 30_000,
    };
    let error = marker::fingerprint::capture::capture_from_source(
        &request,
        &source,
        &control,
        std::ffi::OsStr::new("ffmpeg"),
    )
    .unwrap_err();
    let reason = match case.as_str() {
        "missing" => "ambiguous packet/frame association",
        "malformed" => "incomplete packet JSON",
        "capacity" => "exceeded bounded capacity",
        "overflow" => "end overflow",
        "cancel" => "Library work cancelled",
        _ => unreachable!(),
    };
    assert!(error.message.contains(reason), "{error}");
    assert!(error.metrics.quantized_mapping.is_none());
    assert!(error.metrics.contiguous_pcm_start_ms.is_none());
}
