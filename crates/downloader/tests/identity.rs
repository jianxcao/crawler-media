use downloader::torrent_matches_snapshot;

#[test]
fn episode_numbers_must_not_collide_even_with_same_size() {
    // 两个不同集号的种子，即使体积相同，也绝不能被认定为同一个任务
    assert!(!torrent_matches_snapshot(
        "Show.S01.E01.1080p",
        Some(1000),
        "Show.S01.E02.1080p",
        1000,
    ));

    // 季号不同也不能匹配
    assert!(!torrent_matches_snapshot(
        "Show.S01.E01.1080p",
        Some(1000),
        "Show.S02.E01.1080p",
        1000,
    ));

    // 范围不一致也不能匹配
    assert!(!torrent_matches_snapshot(
        "Show.S01E01-E04",
        Some(1000),
        "Show.S01E01-E05",
        1000,
    ));

    // 相同的季集和标题，标点/点号/大小写变体应正常匹配
    assert!(torrent_matches_snapshot(
        "Show.S01E01.1080p",
        Some(1000),
        "Show.S01.E01.1080p",
        1000,
    ));
    assert!(torrent_matches_snapshot(
        "Show.S01E01.1080p",
        Some(1000),
        "Show S01E01 1080p",
        1000,
    ));

    // Sample 任务绝对不能与正片匹配
    assert!(!torrent_matches_snapshot(
        "Show.S01E01.1080p.Sample",
        Some(1000),
        "Show.S01E01.1080p",
        1000,
    ));
}

#[test]
fn movie_titles_must_not_collide_even_with_same_size() {
    // 两个不同电影（如 Alien 1979 vs Aliens 1979），即使同体积或目标大小未知，也绝不能误匹配
    assert!(!torrent_matches_snapshot(
        "Alien.1979.1080p",
        Some(1000),
        "Aliens.1979.1080p",
        1000,
    ));
    assert!(!torrent_matches_snapshot(
        "Alien.1979.1080p",
        None,
        "Aliens.1979.1080p",
        1000,
    ));

    // 合法的双语命名变体（如包含完整的原英文标题 The Matrix）必须能够正常匹配
    assert!(torrent_matches_snapshot(
        "The.Matrix.1999.1080p",
        Some(1000),
        "黑客帝国.The.Matrix.1999.1080p",
        1000,
    ));

    // 带有同语言英文续作标题的电影（如 Matrix Reloaded、Alien Covenant）绝不能被误判为第一部
    assert!(!torrent_matches_snapshot(
        "The.Matrix.1080p",
        Some(1000),
        "The.Matrix.Reloaded.1080p",
        1000,
    ));
    assert!(!torrent_matches_snapshot(
        "Alien.1080p",
        Some(1000),
        "Alien.Covenant.1080p",
        1000,
    ));
}
