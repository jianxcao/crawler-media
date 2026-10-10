#[test]
fn fanart_prefers_zh_then_keeps_season_images_and_drops_background() {
    let body = include_str!("fixtures/fanart_tv.json");
    let set = media::fanart::parse_fanart(body, &["zh"]).unwrap();
    assert_eq!(
        set.logo.unwrap().url,
        "https://assets.fanart.tv/logo-zh.png"
    );
    assert_eq!(
        set.thumb.unwrap().url,
        "https://assets.fanart.tv/thumb-en.jpg"
    );
    assert_eq!(
        set.banner.unwrap().url,
        "https://assets.fanart.tv/banner-en.jpg"
    );
    assert_eq!(set.season_posters.len(), 1);
    assert_eq!(set.season_posters[0].season, Some(1));
    assert_eq!(
        set.season_posters[0].url,
        "https://assets.fanart.tv/s1-zh.jpg"
    );
    assert_eq!(set.season_thumbs[0].url, "https://assets.fanart.tv/s1-thumb.jpg");
    assert_eq!(set.season_banners[0].season, Some(0));
}

#[test]
fn fanart_error_status_is_an_empty_set() {
    let set = media::fanart::parse_fanart(r#"{"status":"error"}"#, &[]).unwrap();
    assert_eq!(set, media::fanart::FanartSet::default());
}
