use library::{Chapter, MarkerType, ProbeTarget, annotate_chapters, classify_chapter_title};

#[test]
fn classify_chapter_title_identifies_intro_and_credits() {
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
fn annotate_chapters_attaches_markers() {
    let chapters = vec![
        Chapter {
            start_ms: 0,
            end_ms: 90000,
            title: Some("Prologue".into()),
        },
        Chapter {
            start_ms: 90000,
            end_ms: 180000,
            title: Some("Opening Theme".into()),
        },
        Chapter {
            start_ms: 180000,
            end_ms: 1200000,
            title: Some("Episode 1".into()),
        },
        Chapter {
            start_ms: 1200000,
            end_ms: 1290000,
            title: Some("Ending".into()),
        },
    ];
    let annotated = annotate_chapters(&chapters);
    assert_eq!(annotated.len(), 4);
    assert_eq!(annotated[0].marker_type, Some(MarkerType::IntroStart));
    assert_eq!(annotated[1].marker_type, Some(MarkerType::IntroStart));
    assert_eq!(annotated[2].marker_type, None);
    assert_eq!(annotated[3].marker_type, Some(MarkerType::CreditsStart));
}

#[test]
fn probe_target_resolves_strm_first_line_url() {
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
    std::fs::write(&mkv_path, "binary").unwrap();
    let local_target = ProbeTarget::from_path(&mkv_path);
    assert_eq!(local_target, ProbeTarget::Local(mkv_path));
}
