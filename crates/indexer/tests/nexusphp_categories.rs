use std::sync::Arc;

use domain::{Site, SiteId};
use indexer::{FetchMethod, FetchRequest, Fetcher, Indexer, IndexerError, RegistrySnapshot};
use parking_lot::Mutex;

fn nexus_site(profile_id: &str) -> Site {
    Site {
        id: SiteId::new(),
        name: profile_id.into(),
        url: "https://pt.example/".into(),
        profile_id: profile_id.into(),
        cookie: Some("uid=1".into()),
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
        browser_search: false,
        timezone: "UTC".into(),
    }
}

struct RecordingFetcher {
    urls: Mutex<Vec<String>>,
    html: &'static str,
}

impl Fetcher for RecordingFetcher {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        assert_eq!(request.method, FetchMethod::Get);
        self.urls.lock().push(request.url.clone());
        Ok(self.html.into())
    }
}

#[test]
fn nexusphp_single_and_multi_category_search_uses_cat_id_flags() {
    let fetcher = Arc::new(RecordingFetcher {
        urls: Mutex::new(Vec::new()),
        html: include_str!("fixtures/nexusphp.html"),
    });
    let indexer = Indexer::new(RegistrySnapshot::builtin().unwrap(), fetcher.clone());
    let demo = nexus_site("demo");

    let movie =
        indexer.search_page_categories(std::slice::from_ref(&demo), "matrix", 1, &["movie".into()]);
    assert!(movie.failures.is_empty(), "{:?}", movie.failures);

    let tv =
        indexer.search_page_categories(std::slice::from_ref(&demo), "friends", 1, &["tv".into()]);
    assert!(tv.failures.is_empty(), "{:?}", tv.failures);

    let combined_page2 = indexer.search_page_categories(
        std::slice::from_ref(&demo),
        "planet earth",
        2,
        &["movie".into(), "documentary".into()],
    );
    assert!(
        combined_page2.failures.is_empty(),
        "{:?}",
        combined_page2.failures
    );

    let urls = fetcher.urls.lock().clone();
    assert_eq!(
        urls,
        vec![
            "https://pt.example/torrents.php?search=matrix&cat401=1",
            "https://pt.example/torrents.php?search=friends&cat402=1&cat403=1",
            "https://pt.example/torrents.php?search=planet+earth&page=1&cat401=1&cat404=1",
        ]
    );
}

#[test]
fn hdarea_and_standard_nexusphp_profiles_have_verified_category_mappings() {
    let fetcher = Arc::new(RecordingFetcher {
        urls: Mutex::new(Vec::new()),
        html: "<html><body><table class='torrents'></table></body></html>",
    });
    let profiles = RegistrySnapshot::builtin().unwrap();
    let indexer = Indexer::new(profiles.clone(), fetcher.clone());

    let hdarea_site = nexus_site("hdarea");
    let _ = indexer.search_page_categories(
        std::slice::from_ref(&hdarea_site),
        "dune",
        1,
        &["movie".into()],
    );
    let _ = indexer.search_page_categories(
        std::slice::from_ref(&hdarea_site),
        "ost",
        1,
        &["music".into(), "other".into()],
    );
    assert_eq!(
        fetcher.urls.lock().as_slice(),
        &[
            "https://pt.example/torrents.php?search=dune&cat300=1&cat401=1&cat415=1&cat416=1&cat410=1&cat411=1&cat414=1&cat412=1&cat413=1&cat417=1",
            "https://pt.example/torrents.php?search=ost&cat406=1&cat408=1&cat407=1&cat409=1",
        ]
    );

    let all_cats = [
        "movie".into(),
        "tv".into(),
        "documentary".into(),
        "anime".into(),
        "music".into(),
        "other".into(),
    ];
    for id in [
        "demo",
        "agsvpt",
        "audiences",
        "chdbits",
        "hddolby",
        "hdfans",
        "hdhome",
        "hdsky",
        "hdtime",
        "hdvideo",
        "hhanclub",
        "keepfrds",
        "nicept",
        "ourbits",
        "piggo",
        "pterclub",
        "pthome",
        "pttime",
        "soulvoice",
        "ssd",
        "tjupt",
        "ttg",
    ] {
        fetcher.urls.lock().clear();
        let profile = profiles
            .profile(id)
            .unwrap_or_else(|| panic!("missing {id}"));
        assert!(
            profile.categories.is_some(),
            "profile {id} must define categories"
        );
        let site = nexus_site(id);
        let _ = indexer.search_page_categories(std::slice::from_ref(&site), "test", 1, &all_cats);
        let url = fetcher.urls.lock()[0].clone();
        assert_eq!(
            url,
            "https://pt.example/torrents.php?search=test&cat401=1&cat402=1&cat403=1&cat404=1&cat405=1&cat406=1&cat408=1&cat407=1&cat409=1",
            "unexpected category params for {id}"
        );
    }
}

#[test]
fn overlay_nexusphp_profile_numeric_and_custom_prefix_category_ids_encode_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("custom.yaml"),
        r#"
id: custom
framework: nexusphp
search:
  path: /torrents.php
  query_param: search
categories:
  movie: [401, 419]
list:
  item: "tr.torrent"
fields:
  title:
    selector: "a.name"
  enclosure:
    selector: "a.download"
    attr: href
"#,
    )
    .unwrap();
    let fetcher = Arc::new(RecordingFetcher {
        urls: Mutex::new(Vec::new()),
        html: include_str!("fixtures/nexusphp.html"),
    });
    let indexer = Indexer::new(
        RegistrySnapshot::load_with_overlay(Some(dir.path())).unwrap(),
        fetcher.clone(),
    );
    let site = nexus_site("custom");
    let _ = indexer.search_page_categories(
        std::slice::from_ref(&site),
        "blade runner",
        1,
        &["movie".into()],
    );
    assert_eq!(
        fetcher.urls.lock().as_slice(),
        &["https://pt.example/torrents.php?search=blade+runner&cat401=1&cat419=1"]
    );
}
