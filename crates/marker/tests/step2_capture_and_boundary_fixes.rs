//! Step 2 TDD regression suite for core capture PTS/origin alignment, AAC multi-packet
//! priming, disjoint container chapters, audio-shorter-than-container EOF guards,
//! and adaptive multi-window absolute coordinate clustering.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use marker::adaptive::{EpisodeEvidence, SamplingPolicy, SegmentKind, build_season_models};
use marker::boundary::evidence::{
    ReferenceSupport, collect_boundary_evidence, complete_guards, complete_pcm,
};
use marker::boundary::{BoundaryDecision, BoundaryPolicy, Interval, refine_boundary};
use marker::{
    CaptureMetrics, CaptureRequest, CaptureSourceFacts, CapturedFingerprint, CommonSegment,
    FingerprintCaptureEngine, FingerprintEngine, PairFingerprintDecision, PcmTimeline,
    SampleWindow, WorkControl,
};

fn ff(args: &[&str], output: &Path) {
    let res = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-nostdin",
            "-y",
            "-filter_threads",
            "1",
            "-threads",
            "1",
        ])
        .args(args)
        .arg(output)
        .output()
        .unwrap();
    assert!(
        res.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&res.stderr)
    );
}

#[test]
fn sub_millisecond_container_origin_preserves_contiguous_pcm_start_and_completeness() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("base.mp4");
    ff(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=16x16:r=24000/1001:d=15",
            "-f",
            "lavfi",
            "-i",
            "anoisesrc=seed=7:sample_rate=48000:duration=15",
            "-ac",
            "2",
            "-c:v",
            "libx264",
            "-c:a",
            "aac",
        ],
        &raw,
    );
    // Shift timestamps in MPEG-TS (90kHz clock: 0.021333s -> start_time=0.021333, 21333us!)
    let shifted = dir.path().join("subms_origin.ts");
    ff(
        &[
            "-copyts",
            "-itsoffset",
            "0.021333",
            "-i",
            raw.to_str().unwrap(),
            "-muxpreload",
            "0",
            "-muxdelay",
            "0",
            "-c",
            "copy",
        ],
        &shifted,
    );

    let head = marker::ChromaprintEngine
        .capture_window(&CaptureRequest {
            path: shifted.clone(),
            window: SampleWindow::new(0, 10_000),
            audio_stream_index: None,
            process_deadline_ms: 30_000,
        })
        .unwrap();
    assert_eq!(head.metrics.pcm_timeline, PcmTimeline::Contiguous);
    assert_eq!(
        head.metrics.contiguous_pcm_start_ms,
        Some(0),
        "sub-ms container start_time_us ({:?}) must still resolve contiguous_pcm_start_ms",
        head.metrics.source_facts
    );
    assert!(complete_pcm(&head));

    let interior = marker::ChromaprintEngine
        .capture_window(&CaptureRequest {
            path: shifted,
            window: SampleWindow::new(3_000, 11_000),
            audio_stream_index: None,
            process_deadline_ms: 30_000,
        })
        .unwrap();
    assert_eq!(interior.metrics.pcm_timeline, PcmTimeline::Contiguous);
    assert!(
        interior
            .metrics
            .contiguous_pcm_start_ms
            .is_some_and(|start| (start - 3_000).abs() <= 50),
        "interior capture on sub-ms origin container had start={:?}",
        interior.metrics.contiguous_pcm_start_ms
    );
    assert!(complete_pcm(&interior));
}

fn prepare_apple_priming_probe_json(mka: &Path, proof_path: &Path) {
    let probe_out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-threads",
            "1",
            "-select_streams",
            "0",
            "-read_intervals",
            "%6.000000",
            "-show_packets",
            "-show_frames",
            "-show_streams",
            "-show_format",
            "-show_data",
            "-show_entries",
            "packet=stream_index,pts,pos,size,data:packet_side_data=side_data_type,skip_samples,discard_padding:frame=stream_index,pts,pkt_pos,pkt_size,nb_samples:stream=index,codec_name,profile,sample_rate,channels,time_base,extradata,initial_padding:format=format_name,start_time",
            "-of",
            "json",
        ])
        .arg(mka)
        .output()
        .unwrap();
    assert!(probe_out.status.success());
    let mut json: serde_json::Value = serde_json::from_slice(&probe_out.stdout).unwrap();
    json["streams"][0]["initial_padding"] = serde_json::json!(2112);
    json["streams"][0]["extradata"] =
        serde_json::json!("00000000: 1190 56e5 0000 00                        ..V....\n");

    let items = json["packets_and_frames"].as_array_mut().unwrap();
    let pkt_indices = items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| (item["type"].as_str() == Some("packet")).then_some(idx))
        .collect::<Vec<_>>();
    let pkt0_pos = items[pkt_indices[0]]["pos"].clone();
    let pkt1_pos = items[pkt_indices[1]]["pos"].clone();
    let pkt2_pos = items[pkt_indices[2]]["pos"].clone();
    for (&idx, skip) in [
        (&pkt_indices[0], 1024),
        (&pkt_indices[1], 1024),
        (&pkt_indices[2], 64),
    ] {
        items[idx]["side_data_list"] = serde_json::json!([{
            "side_data_type": "Skip Samples",
            "skip_samples": skip,
            "discard_padding": 0
        }]);
    }
    items.retain(|item| {
        item["type"] != "frame" || (item["pkt_pos"] != pkt0_pos && item["pkt_pos"] != pkt1_pos)
    });
    for item in items.iter_mut() {
        if item["type"] == "frame" && item["pkt_pos"] == pkt2_pos {
            item["nb_samples"] = serde_json::json!(960);
        }
    }
    std::fs::write(proof_path, serde_json::to_vec(&json).unwrap()).unwrap();
}

#[test]
fn quantized_aac_accepts_multi_packet_apple_priming_and_extended_asc() {
    if std::env::var("CM_STEP2_APPLE_CHILD").is_err() {
        let dir = tempfile::tempdir().unwrap();
        let mka = dir.path().join("primed.mka");
        ff(
            &[
                "-f",
                "lavfi",
                "-i",
                "anoisesrc=seed=19:sample_rate=48000:duration=8",
                "-ac",
                "2",
                "-c:a",
                "aac",
            ],
            &mka,
        );
        let proof_path = dir.path().join("apple_priming.json");
        prepare_apple_priming_probe_json(&mka, &proof_path);
        let fake_ffprobe = dir.path().join("ffprobe");
        std::fs::write(
            &fake_ffprobe,
            format!("#!/bin/sh\ncat '{}'\n", proof_path.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_ffprobe, std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let exe = std::env::current_exe().unwrap();
        let out = Command::new(exe)
            .args([
                "--exact",
                "quantized_aac_accepts_multi_packet_apple_priming_and_extended_asc",
                "--nocapture",
            ])
            .env("CM_STEP2_APPLE_CHILD", "1")
            .env("CM_STEP2_MKA", &mka)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    dir.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "child failed: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let target_mka = std::path::PathBuf::from(std::env::var("CM_STEP2_MKA").unwrap());
    let source = CaptureSourceFacts {
        audio_stream_index: 0,
        format_start_time_us: Some(0),
        stream_start_pts: Some(0),
        stream_time_base: Some("1/1000".into()),
        codec_name: Some("aac".into()),
        native_sample_rate: Some(48_000),
        audio_channels: Some(2),
        provenance: "fixture".into(),
    };
    let captured = marker::fingerprint::capture::capture_from_source(
        &CaptureRequest {
            path: target_mka,
            window: SampleWindow::new(0, 5_000),
            audio_stream_index: Some(0),
            process_deadline_ms: 30_000,
        },
        &source,
        &WorkControl::default(),
        std::ffi::OsStr::new("ffmpeg"),
    )
    .expect("Apple 2112 priming and extended LC extradata should succeed");
    assert_eq!(captured.metrics.pcm_timeline, PcmTimeline::Contiguous);
    assert!(captured.metrics.quantized_mapping.is_some());
}

#[test]
fn single_whole_file_dummy_chapter_and_adjacent_scene_chapters_do_not_trigger_conflicting_evidence()
{
    let mut rms = vec![-60_000i32; 9_000];
    rms[1_510..5_990].fill(-15_000);
    let sample = CapturedFingerprint {
        window: SampleWindow::new(1_320_000, 1_500_000),
        words: vec![10, 20, 30, 40],
        pcm_duration_ms: Some(180_000),
        metrics: CaptureMetrics {
            time_mapping_version: 3,
            pcm_timeline: PcmTimeline::Contiguous,
            contiguous_pcm_start_ms: Some(1_320_000),
            pcm_frame_ms: 20,
            pcm_rms_dbfs_milli: rms,
            ..Default::default()
        },
    };
    let outro_candidate = Interval {
        start_ms: 1_350_000,
        end_ms: 1_440_000,
    };
    // Case 1: MKVToolNix / HandBrake single whole-file dummy chapter [0..1_500_000]
    let dummy_chapters = vec![Interval {
        start_ms: 0,
        end_ms: 1_500_000,
    }];
    let evidence = collect_boundary_evidence(
        &sample,
        outro_candidate,
        &dummy_chapters,
        SegmentKind::Outro,
        Some(1_500_000),
        ReferenceSupport {
            references_agree: true,
            complete_guards: true,
        },
        &BoundaryPolicy::default(),
    );
    assert!(
        !evidence.conflicting_evidence,
        "single whole-file muxer dummy chapter must not flag conflicting_evidence"
    );
    assert_eq!(
        refine_boundary(&evidence, &BoundaryPolicy::default()),
        BoundaryDecision::Verified(outro_candidate)
    );

    // Case 2: Full-timeline scene chapters containing adjacent chapters + corroborating ED chapter
    let full_chapters = vec![
        Interval {
            start_ms: 30_000,
            end_ms: 90_000,
        },
        Interval {
            start_ms: 90_000,
            end_ms: 1_350_000,
        },
        Interval {
            start_ms: 1_350_200,
            end_ms: 1_439_800,
        },
        Interval {
            start_ms: 1_439_800,
            end_ms: 1_500_000,
        },
    ];
    let evidence2 = collect_boundary_evidence(
        &sample,
        outro_candidate,
        &full_chapters,
        SegmentKind::Outro,
        Some(1_500_000),
        ReferenceSupport {
            references_agree: true,
            complete_guards: true,
        },
        &BoundaryPolicy::default(),
    );
    assert!(!evidence2.conflicting_evidence);
    assert_eq!(
        refine_boundary(&evidence2, &BoundaryPolicy::default()),
        BoundaryDecision::Verified(Interval {
            start_ms: 1_350_200,
            end_ms: 1_439_800,
        })
    );
}

#[test]
fn audio_stream_slightly_shorter_than_video_container_at_eof_passes_completeness_and_guards() {
    // Video container is 1_500_000ms, audio stream cleanly ended at 1_499_760ms (240ms shorter).
    let tail = CapturedFingerprint {
        window: SampleWindow::new(1_320_000, 1_500_000),
        words: vec![1, 2, 3, 4],
        pcm_duration_ms: Some(179_760),
        metrics: CaptureMetrics {
            time_mapping_version: 3,
            pcm_timeline: PcmTimeline::Contiguous,
            contiguous_pcm_start_ms: Some(1_320_000),
            ffmpeg_exit_code: Some(0),
            ..Default::default()
        },
    };
    assert!(
        !complete_pcm(&tail),
        "short PCM without duration context must not relax whole-window absence proof"
    );
    assert!(
        complete_guards(
            &tail,
            Interval {
                start_ms: 1_410_000,
                end_ms: 1_499_760,
            },
            SegmentKind::Outro,
            Some(1_500_000),
            &BoundaryPolicy::default(),
        ),
        "full-bleed outro ending at physical audio EOF (240ms before video container end) must pass complete_guards"
    );
}

struct MultiWindowEngine;

impl FingerprintEngine for MultiWindowEngine {
    fn extract_at(
        &self,
        _path: &Path,
        _start: u32,
        _dur: u32,
        _ctrl: &WorkControl,
    ) -> Result<Vec<u32>, String> {
        Ok(vec![1; 128])
    }

    fn find_common_segment(
        &self,
        _first: &[u32],
        _second: &[u32],
        _min: f32,
        _max: f32,
    ) -> Option<CommonSegment> {
        None
    }

    fn compare_evidence(
        &self,
        first: &EpisodeEvidence,
        second: &EpisodeEvidence,
        _min: f32,
        _max: f32,
    ) -> PairFingerprintDecision {
        // Intro is at absolute [130_000..160_000] in all 3 episodes!
        // Ep1 has sample in [0..180_000] (rel 130..160) AND sample in [120_000..420_000] (rel 10..40).
        // Ep2 only matches Ep1's first window; Ep3 only matches Ep1's second window.
        let pair = (first.sample_id.as_str(), second.sample_id.as_str());
        let mk = |s1: f32, e1: f32, s2: f32, e2: f32| {
            PairFingerprintDecision::Candidates(vec![CommonSegment {
                start1_sec: s1,
                end1_sec: e1,
                start2_sec: s2,
                end2_sec: e2,
                duration_sec: e1 - s1,
                score: 1.5,
            }])
        };
        match pair {
            ("ep1-w1", "ep2-w1") => mk(130.0, 160.0, 130.0, 160.0),
            ("ep1-w2", "ep3-w2") => mk(10.0, 40.0, 10.0, 40.0),
            _ => PairFingerprintDecision::NoMatch,
        }
    }
}

#[test]
fn adaptive_model_clusters_across_distinct_windows_using_absolute_coordinates() {
    let mk_ev =
        |sample_id: &str, ledger_id: &str, ep: u32, start_ms: i64, end_ms: i64| EpisodeEvidence {
            sample_id: sample_id.into(),
            ledger_id: ledger_id.into(),
            episode: ep,
            source_version: "v1".into(),
            capture_profile_key: "prof-v3".into(),
            kind: SegmentKind::Intro,
            duration_ms: Some(1_500_000),
            capture: CapturedFingerprint {
                window: SampleWindow::new(start_ms, end_ms),
                words: (0..256).map(|i| (i as u32).wrapping_mul(31) ^ ep).collect(),
                pcm_duration_ms: Some(end_ms - start_ms),
                metrics: CaptureMetrics {
                    time_mapping_version: 3,
                    pcm_timeline: PcmTimeline::Contiguous,
                    contiguous_pcm_start_ms: Some(start_ms),
                    ..Default::default()
                },
            },
        };
    let evidences = vec![
        mk_ev("ep1-w1", "file-1", 1, 0, 180_000),
        mk_ev("ep1-w2", "file-1", 1, 120_000, 420_000),
        mk_ev("ep2-w1", "file-2", 2, 0, 180_000),
        mk_ev("ep3-w2", "file-3", 3, 120_000, 420_000),
    ];
    let models = build_season_models(&MultiWindowEngine, &evidences, &SamplingPolicy::default());
    let _ = HashMap::<String, ()>::new();
    assert_eq!(
        models.len(),
        1,
        "expected 1 unified cluster, got {models:?}"
    );
    assert!(
        models[0].is_stable,
        "cluster spanning ep1+ep2+ep3 across w1/w2 must be stable (3 unique episodes)"
    );
}
