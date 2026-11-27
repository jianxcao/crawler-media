#[test]
fn parse_preserves_multi_episode_codec_suffixed_and_token_fallback_versioned_episodes() {
    let r1 = release::parse("Show.Name.E01E02.1080p.WEB-DL.mkv");
    assert_eq!(r1.title, "Show Name");
    assert_eq!(
        (r1.season, r1.episode, r1.episode_to),
        (Some(1), Some(1), Some(2))
    );

    let r2 = release::parse("[SubGroup] E01E02.1080p.mkv");
    assert_eq!(
        (r2.season, r2.episode, r2.episode_to),
        (Some(1), Some(1), Some(2))
    );

    let r2b = release::parse("[SubGroup] Show Name E01E02.1080p.mkv");
    assert_eq!(r2b.title, "Show Name");
    assert_eq!(
        (r2b.season, r2b.episode, r2b.episode_to),
        (Some(1), Some(1), Some(2))
    );

    let r3 = release::parse("Show.Name.E01x264.1080p.mkv");
    assert_eq!(r3.title, "Show Name");
    assert_eq!(
        (r3.season, r3.episode, r3.episode_to),
        (Some(1), Some(1), None)
    );

    let r4 = release::parse("Show.Name.S01E01E02.1080p.WEB-DL.mkv");
    assert_eq!(r4.title, "Show Name");
    assert_eq!(
        (r4.season, r4.episode, r4.episode_to),
        (Some(1), Some(1), Some(2))
    );

    let r5 = release::parse("Show.Name.S01E01v2E02v2.1080p.WEB-DL.mkv");
    assert_eq!(r5.title, "Show Name");
    assert_eq!(
        (r5.season, r5.episode, r5.episode_to),
        (Some(1), Some(1), Some(2))
    );

    let r6 = release::parse("Show.Name.S01E01x264.mkv");
    assert_eq!(r6.title, "Show Name");
    assert_eq!(
        (r6.season, r6.episode, r6.episode_to),
        (Some(1), Some(1), None)
    );

    let r7 = release::parse("Show Episode 01v2 1080p.mkv");
    assert_eq!(
        (r7.season, r7.episode, r7.episode_to),
        (Some(1), Some(1), None)
    );

    let r8 = release::parse("[SubGroup] S01 01v2-02v2 1080p.mkv");
    assert_eq!(
        (r8.season, r8.episode, r8.episode_to),
        (Some(1), Some(1), Some(2))
    );
}

#[test]
fn parse_handles_three_plus_chained_eps_glued_codecs_and_rejects_inverted_chains() {
    let c1 = release::parse("[SubGroup] E01E02E03.1080p.mkv");
    assert_eq!(
        (c1.season, c1.episode, c1.episode_to),
        (Some(1), Some(1), Some(3))
    );

    let c2 = release::parse("[SubGroup] S01E01E02E03.1080p.mkv");
    assert_eq!(
        (c2.season, c2.episode, c2.episode_to),
        (Some(1), Some(1), Some(3))
    );

    let av1 = release::parse("Show.Name.S01E01AV1.1080p.mkv");
    assert_eq!(av1.title, "Show Name");
    assert_eq!(
        (av1.season, av1.episode, av1.episode_to),
        (Some(1), Some(1), None)
    );

    let h264 = release::parse("Show.Name.S01E01H.264.mkv");
    assert_eq!(h264.title, "Show Name");
    assert_eq!(
        (h264.season, h264.episode, h264.episode_to),
        (Some(1), Some(1), None)
    );

    let h264_tok = release::parse("[SubGroup] S01E01H.264.mkv");
    assert_eq!(
        (h264_tok.season, h264_tok.episode, h264_tok.episode_to),
        (Some(1), Some(1), None)
    );

    let range_codec = release::parse("[SubGroup] S01E01-02x264.mkv");
    assert_eq!(
        (
            range_codec.season,
            range_codec.episode,
            range_codec.episode_to
        ),
        (Some(1), Some(1), Some(2))
    );

    let bad_zero = release::parse("Show.Name.S01E05E00.1080p.mkv");
    assert_ne!(bad_zero.episode_to, Some(0));
    assert_eq!((bad_zero.episode, bad_zero.episode_to), (None, None));

    let bad_inv = release::parse("Show.Name.S01E05E02.1080p.mkv");
    assert_eq!((bad_inv.episode, bad_inv.episode_to), (None, None));
}
