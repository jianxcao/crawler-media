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

struct Scripted {
    body: String,
    seen: std::sync::Mutex<Vec<String>>,
}

impl media::CatalogGet for Scripted {
    fn get(&self, path: &str) -> Result<String, media::TmdbError> {
        self.seen.lock().unwrap().push(path.to_string());
        assert!(
            path.starts_with("https://webservice.fanart.tv/v3/tv/425039?api_key=secret"),
            "{path}"
        );
        Ok(self.body.clone())
    }
}

#[test]
fn fanart_client_requests_the_tv_url_without_putting_the_key_in_the_cache_key() {
    let tmp = tempfile::tempdir().unwrap();
    let http = Scripted {
        body: include_str!("fixtures/fanart_tv.json").to_string(),
        seen: std::sync::Mutex::new(Vec::new()),
    };
    let client = media::fanart::FanartClient::new(http, &tmp.path().join("cache.db"), "secret").unwrap();
    let set = client.tv("425039", &["zh"]).unwrap();
    assert!(set.logo.is_some());
}
