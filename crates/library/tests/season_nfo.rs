use library::{NfoMeta, read_nfo, write_season_nfo};

#[test]
fn write_season_nfo_creates_season_root_with_plot_and_number() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("season.nfo");
    let meta = NfoMeta {
        title: Some("第 1 季".into()),
        plot: Some("第一季简介".into()),
        premiered: Some("2022-10-10".into()),
        thumb: Some("https://image.tmdb.org/t/p/w780/season.jpg".into()),
        season: Some(1),
        ..NfoMeta::default()
    };
    write_season_nfo(&path, &meta).unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("<season>"), "{body}");
    assert!(body.contains("<seasonnumber>1</seasonnumber>"), "{body}");
    assert!(body.contains("<year>2022</year>"), "{body}");
    let parsed = read_nfo(&path).unwrap();
    assert_eq!(parsed.title.as_deref(), Some("第 1 季"));
    assert_eq!(parsed.plot.as_deref(), Some("第一季简介"));
    assert_eq!(parsed.premiered.as_deref(), Some("2022-10-10"));
    assert_eq!(parsed.season, Some(1));
    assert_eq!(
        parsed.thumb.as_deref(),
        Some("https://image.tmdb.org/t/p/w780/season.jpg")
    );
}

#[test]
fn write_season_nfo_refuses_to_replace_tvshow_root() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("tvshow.nfo");
    std::fs::write(&path, "<tvshow><title>keep</title></tvshow>").unwrap();
    let error = write_season_nfo(&path, &NfoMeta::default()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(std::fs::read_to_string(&path).unwrap().contains("keep"));
}
