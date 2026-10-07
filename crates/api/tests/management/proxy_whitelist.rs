//! Unit tests for http_agent proxy whitelist.

use api::http_agent::should_use_proxy_for_url;

#[test]
fn test_proxy_whitelist_domains() {
    // Whitelisted: TMDB, TVDB, AniList, Bangumi, TheIntroDB
    assert!(should_use_proxy_for_url(
        "https://api.themoviedb.org/3/movie/550"
    ));
    assert!(should_use_proxy_for_url(
        "https://image.tmdb.org/t/p/w500/xyz.jpg"
    ));
    assert!(should_use_proxy_for_url(
        "https://api4.thetvdb.com/v4/series/1234"
    ));
    assert!(should_use_proxy_for_url("https://graphql.anilist.co"));
    assert!(should_use_proxy_for_url("https://api.bgm.tv/subject/123"));
    assert!(should_use_proxy_for_url(
        "https://api.theintrodb.org/v3/shows/123"
    ));

    // Excluded: Douban (always bypass by default)
    assert!(!should_use_proxy_for_url(
        "https://api.douban.com/v2/movie/1234"
    ));
    assert!(!should_use_proxy_for_url(
        "https://img3.doubanio.com/view/photo/s_ratio_poster/public/p123.jpg"
    ));

    // Excluded: Test and local domains
    assert!(!should_use_proxy_for_url("https://example.com/video.mp4"));
    assert!(!should_use_proxy_for_url(
        "http://cdn.example.com/stream/file.mkv"
    ));
    assert!(!should_use_proxy_for_url("http://localhost:8080/stream"));
    assert!(!should_use_proxy_for_url("http://127.0.0.1:3334/api"));

    // Excluded: Cloud drives and STRM stream providers (115, aliyun, etc.)
    assert!(!should_use_proxy_for_url(
        "https://proapi.115.com/files/download"
    ));
    assert!(!should_use_proxy_for_url(
        "https://v.anxia.com/stream/123.mkv"
    ));
    assert!(!should_use_proxy_for_url(
        "https://alipan.com/drive/video.mp4"
    ));
}
