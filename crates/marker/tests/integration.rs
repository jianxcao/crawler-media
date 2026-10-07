use marker::{
    Chapter, MarkerType, ProbeTarget, annotate_chapters, classify_chapter_title,
    find_common_segment, match_episodes_fingerprints,
};
use rusty_chromaprint::{Configuration, Fingerprinter};
use std::process::Command;

#[test]
fn test_classify_chapter_title() {
    assert_eq!(
        classify_chapter_title("Intro"),
        Some(MarkerType::IntroStart)
    );
    assert_eq!(
        classify_chapter_title("opening"),
        Some(MarkerType::IntroStart)
    );
    assert_eq!(classify_chapter_title("OP 1"), Some(MarkerType::IntroStart));
    assert_eq!(
        classify_chapter_title("片头曲"),
        Some(MarkerType::IntroStart)
    );
    assert_eq!(classify_chapter_title("序幕"), Some(MarkerType::IntroStart));
    assert_eq!(
        classify_chapter_title("オープニング"),
        Some(MarkerType::IntroStart)
    );

    assert_eq!(
        classify_chapter_title("Ending"),
        Some(MarkerType::CreditsStart)
    );
    assert_eq!(classify_chapter_title("ED"), Some(MarkerType::CreditsStart));
    assert_eq!(
        classify_chapter_title("Credits"),
        Some(MarkerType::CreditsStart)
    );
    assert_eq!(
        classify_chapter_title("片尾"),
        Some(MarkerType::CreditsStart)
    );
    assert_eq!(
        classify_chapter_title("演职员表"),
        Some(MarkerType::CreditsStart)
    );

    assert_eq!(classify_chapter_title("Chapter 1"), None);
    assert_eq!(classify_chapter_title("Main Feature"), None);
}

#[test]
fn test_annotate_chapters() {
    let chapters = vec![
        Chapter {
            start_ms: 0,
            end_ms: 90000,
            title: Some("OP Theme".into()),
        },
        Chapter {
            start_ms: 90000,
            end_ms: 1200000,
            title: Some("Main Feature".into()),
        },
        Chapter {
            start_ms: 1200000,
            end_ms: 1300000,
            title: Some("Ending Song".into()),
        },
    ];

    let annotated = annotate_chapters(&chapters);
    assert_eq!(annotated.len(), 3);
    assert_eq!(annotated[0].marker_type, Some(MarkerType::IntroStart));
    assert_eq!(annotated[1].marker_type, None);
    assert_eq!(annotated[2].marker_type, Some(MarkerType::CreditsStart));
}

#[test]
fn test_probe_target_strm_reading() {
    let tmp = tempfile::tempdir().unwrap();
    let strm_path = tmp.path().join("test.strm");
    std::fs::write(
        &strm_path,
        "\n\n  https://example.com/video.mkv?token=123 \nother content",
    )
    .unwrap();

    let target = ProbeTarget::from_path(&strm_path);
    assert_eq!(
        target,
        ProbeTarget::Remote("https://example.com/video.mkv?token=123".into())
    );

    let mkv_path = tmp.path().join("test.mkv");
    std::fs::write(&mkv_path, b"dummy").unwrap();
    let local_target = ProbeTarget::from_path(&mkv_path);
    assert_eq!(local_target, ProbeTarget::Local(mkv_path));
}

#[test]
fn test_probe_target_ffmpeg_command_args() {
    let tmp = tempfile::tempdir().unwrap();
    let strm = tmp.path().join("episode.strm");
    std::fs::write(&strm, "https://cdn.example.test/episode.mkv\n").unwrap();

    let target = ProbeTarget::from_path(&strm);
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-ss", "0", "-t", "60"]);
    target.apply_ffmpeg_input(&mut cmd);
    cmd.args(["-vn", "-ac", "1", "-ar", "16000", "-f", "s16le", "-"]);

    let args: Vec<String> = cmd
        .get_args()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();

    let timeout_idx = args.iter().position(|a| a == "-timeout");
    let input_idx = args.iter().position(|a| a == "-i");
    assert!(
        timeout_idx.is_some(),
        "-timeout must be present for remote STRM"
    );
    assert!(input_idx.is_some(), "-i must be present");
    assert!(
        timeout_idx.unwrap() < input_idx.unwrap(),
        "-timeout ({:?}) must precede -i ({:?})",
        timeout_idx,
        input_idx
    );
}

#[test]
fn test_fingerprint_matching_synthesized_signal() {
    let config = Configuration::preset_test2();

    let mut fp1 = Fingerprinter::new(&config);
    fp1.start(16000, 1).unwrap();

    let mut fp2 = Fingerprinter::new(&config);
    fp2.start(16000, 1).unwrap();

    // 40 秒 440Hz 正弦波作为片头
    let mut common_samples = Vec::new();
    for i in 0..(16000 * 40) {
        let sample =
            (f32::sin(2.0 * std::f32::consts::PI * 440.0 * (i as f32) / 16000.0) * 10000.0) as i16;
        common_samples.push(sample);
    }

    // 第 1 集：前 10 秒静音，紧接着 40 秒片头，后 20 秒不同音频
    let mut ep1_samples = vec![0i16; 16000 * 10];
    ep1_samples.extend_from_slice(&common_samples);
    for i in 0..(16000 * 20) {
        let sample =
            (f32::sin(2.0 * std::f32::consts::PI * 880.0 * (i as f32) / 16000.0) * 8000.0) as i16;
        ep1_samples.push(sample);
    }

    // 第 2 集：前 5 秒静音，紧接着 40 秒片头，后 20 秒不同音频
    let mut ep2_samples = vec![0i16; 16000 * 5];
    ep2_samples.extend_from_slice(&common_samples);
    for i in 0..(16000 * 20) {
        let sample =
            (f32::sin(2.0 * std::f32::consts::PI * 1200.0 * (i as f32) / 16000.0) * 8000.0) as i16;
        ep2_samples.push(sample);
    }

    fp1.consume(&ep1_samples);
    fp1.finish();

    fp2.consume(&ep2_samples);
    fp2.finish();

    let f1 = fp1.fingerprint();
    let f2 = fp2.fingerprint();

    assert!(!f1.is_empty());
    assert!(!f2.is_empty());

    let matched = find_common_segment(f1, f2, 15.0, 60.0);
    assert!(matched.is_some(), "Should find the common segment");
    let seg = matched.unwrap();
    println!("seg: {:?}", seg);
    assert!(
        seg.duration_sec >= 30.0,
        "Matched duration should be close to 40s"
    );

    // 测试 match_episodes_fingerprints 跨集匹配
    let episodes = vec![(1, f1.to_vec()), (2, f2.to_vec())];
    let intros = match_episodes_fingerprints(&episodes, 15.0, 60.0);
    assert_eq!(intros.len(), 2);
    assert_eq!(intros[0].episode, 1);
    assert_eq!(intros[1].episode, 2);
    assert_eq!(intros[0].intro_start_ms, (seg.start1_sec * 1000.0) as i64);
    assert_eq!(intros[1].intro_start_ms, (seg.start2_sec * 1000.0) as i64);
    assert_eq!(intros[0].intro_end_ms, (seg.end1_sec * 1000.0) as i64);
    assert_eq!(intros[1].intro_end_ms, (seg.end2_sec * 1000.0) as i64);
}

#[test]
fn test_build_complete_timeline_chapters() {
    use marker::build_complete_timeline_chapters;

    // 情况 1：片头从 0 开始到 104 秒，没有片尾，总时长 43 分钟 (2580 秒)
    // 应该切成：[0~104s] 片头 + [104s~2580s] 正片
    let segs = build_complete_timeline_chapters(&[], Some((0, 104_000)), None, Some(2580_000));
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].title.as_deref(), Some("片头"));
    assert_eq!(segs[0].start_ms, 0);
    assert_eq!(segs[0].end_ms, 104_000);
    assert_eq!(segs[0].marker_type, Some(MarkerType::IntroStart));

    assert_eq!(segs[1].title.as_deref(), Some("正片"));
    assert_eq!(segs[1].start_ms, 104_000);
    assert_eq!(segs[1].end_ms, 2580_000);
    assert_eq!(segs[1].marker_type, None);

    // 情况 2：冷开场 2 分钟，接着片头 90 秒，片尾在最后 2 分钟，总时长 45 分钟 (2700 秒)
    // 应该切成：[0~120s] 序幕 + [120s~210s] 片头 + [210s~2580s] 正片 + [2580s~2700s] 片尾
    let segs2 = build_complete_timeline_chapters(
        &[],
        Some((120_000, 210_000)),
        Some((2580_000, 2700_000)),
        Some(2700_000),
    );
    assert_eq!(segs2.len(), 4);
    assert_eq!(segs2[0].title.as_deref(), Some("序幕"));
    assert_eq!(segs2[0].start_ms, 0);
    assert_eq!(segs2[0].end_ms, 120_000);

    assert_eq!(segs2[1].title.as_deref(), Some("片头"));
    assert_eq!(segs2[1].start_ms, 120_000);
    assert_eq!(segs2[1].end_ms, 210_000);
    assert_eq!(segs2[1].marker_type, Some(MarkerType::IntroStart));

    assert_eq!(segs2[2].title.as_deref(), Some("正片"));
    assert_eq!(segs2[2].start_ms, 210_000);
    assert_eq!(segs2[2].end_ms, 2580_000);

    assert_eq!(segs2[3].title.as_deref(), Some("片尾"));
    assert_eq!(segs2[3].start_ms, 2580_000);
    assert_eq!(segs2[3].end_ms, 2700_000);
    assert_eq!(segs2[3].marker_type, Some(MarkerType::CreditsStart));
}
