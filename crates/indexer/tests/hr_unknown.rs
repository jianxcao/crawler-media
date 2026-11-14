use std::sync::Arc;

use domain::{Site, SiteId};
use indexer::{FetchRequest, Fetcher, Indexer, IndexerError, ProfileSet};
use serde_json::{Value, json};

struct Injected(String);
impl Fetcher for Injected {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Ok(self.0.clone())
    }
}

fn site(profile: &str) -> Site {
    Site {
        id: SiteId::new(),
        name: profile.into(),
        url: "https://pt.example/".into(),
        profile_id: profile.into(),
        cookie: None,
        api_key: Some("offline".into()),
        rss_url: Some("https://pt.example/rss".into()),
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
    }
}

#[test]
fn mteam_preserves_unknown_and_known_hr_states() {
    for expected in [Value::Null, json!(false), json!(true)] {
        let mut item = json!({"id": "1", "name": "Movie.2026.1080p.WEB-DL", "status": {}});
        if !expected.is_null() {
            item["status"]["hr"] = expected.clone();
        }
        let body = json!({"code": "0", "data": {"data": [item]}}).to_string();
        let indexer = Indexer::new(ProfileSet::load(None).unwrap(), Arc::new(Injected(body)));
        let outcome = indexer.search(&[site("mteam")], "Movie");
        assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
        assert_eq!(
            serde_json::to_value(outcome.torrents[0].hr).unwrap(),
            expected
        );
    }
}

#[test]
fn rss_missing_hr_element_remains_unknown() {
    let body = "<rss><channel><item><title>Movie.2026.1080p.WEB-DL</title><enclosure url='https://pt.example/download/1'/></item></channel></rss>";
    let indexer = Indexer::new(
        ProfileSet::load(None).unwrap(),
        Arc::new(Injected(body.into())),
    );
    let outcome = indexer.rss(&[site("demo")]);
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(
        serde_json::to_value(outcome.torrents[0].hr).unwrap(),
        Value::Null
    );
}

#[test]
fn html_hr_requires_a_valid_configured_selector() {
    for (field, expected) in [
        ("", Value::Null),
        ("  hr:\n    selector: '['\n", Value::Null),
        ("  hr:\n    selector: 'img.hr'\n", json!(false)),
        ("  hr:\n    selector: 'a.name'\n", json!(true)),
    ] {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let profile = format!(
            "id: hr-test\nframework: nexusphp\nsearch:\n  path: /torrents.php\n  query_param: search\nlist:\n  item: tr\nfields:\n  title:\n    selector: a.name\n  enclosure:\n    selector: a.download\n    attr: href\n{field}"
        );
        std::fs::write(temp.path().join("hr-test.yaml"), profile).unwrap();
        let profiles = ProfileSet::load(Some(temp.path())).unwrap();
        let body = "<table><tr><td><a class='name'>Movie.2026.1080p.WEB-DL</a><a class='download' href='/download/1'>download</a></td></tr></table>";
        let indexer = Indexer::new(profiles, Arc::new(Injected(body.into())));
        let outcome = indexer.search(&[site("hr-test")], "Movie");
        assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
        assert_eq!(
            serde_json::to_value(outcome.torrents[0].hr).unwrap(),
            expected
        );
    }
}
