//! GET searches keep serving results while protected, without history writes.

use super::*;
use api::catalog::Catalog;

struct SearchCatalog(domain::Media);

impl Catalog for SearchCatalog {
    fn source_name(&self) -> &'static str {
        "fixture"
    }
    fn source(&self, name: &str) -> Option<&dyn Catalog> {
        (name == "fixture").then_some(self)
    }
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![media::CatalogHit {
            media: self.0.clone(),
            poster_path: None,
            backdrop_path: None,
            rating: None,
            overview: None,
        }])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn details(&self, _: domain::MediaKind, _: &str) -> Result<Option<domain::Media>, String> {
        Ok(None)
    }
}

fn search_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    let media = fixture
        .state
        .store()
        .get_media(fixture.media_id)
        .unwrap()
        .unwrap();
    fixture.state = fixture.state.with_catalog(Arc::new(SearchCatalog(media)));
    let app = router(fixture.state.clone());
    runtime().block_on(common::create_site(&app));
    fixture
}

fn snapshots(fixture: &Fixture) -> Vec<(String, String)> {
    let db = rusqlite::Connection::open(fixture.tmp.path().join("data/app.db")).unwrap();
    db.prepare("SELECT id, payload FROM search_snapshots ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

async fn search_results(app: &axum::Router, kind: &str) {
    let uri = match kind {
        "titles" => "/api/v1/search/titles?keyword=matrix&provider=fixture",
        "torrents" => "/api/v1/search/torrents?keyword=matrix&save_history=true",
        "stream" => "/api/v1/search/torrents/stream?keyword=matrix&save_history=true",
        _ => unreachable!(),
    };
    let response = app
        .clone()
        .oneshot(request("GET", uri, Some(TOKEN), Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    if kind == "stream" {
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let events = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            events.contains("The.Matrix.1999.2160p.BluRay.x265-GROUP"),
            "{events}"
        );
        assert!(events.contains("event: done"), "{events}");
    } else {
        let body = json_body(response).await;
        let (key, expected) = if kind == "titles" {
            ("titles", "Gate fixture")
        } else {
            ("items", "The.Matrix.1999.2160p.BluRay.x265-GROUP")
        };
        assert_eq!(body["data"][key][0]["title"], expected, "{uri}: {body}");
    }
}

fn assert_protected_search(readonly: bool) {
    // A fresh fixture for each seam also verifies that no orphan snapshot is created.
    for kind in ["titles", "torrents", "stream"] {
        let fixture = search_fixture();
        let before = snapshots(&fixture);
        let gate = fixture.state.write_gate();
        let closed = if readonly { None } else { Some(gate.close()) };
        let app = router(fixture.state.clone().with_instance_writable(!readonly));
        runtime().block_on(search_results(&app, kind));
        fixture.assert_unchanged();
        assert_eq!(snapshots(&fixture), before, "{kind}, readonly={readonly}");

        // Opening admission restores normal history + snapshot persistence.
        drop(closed);
        fixture.state.clone().with_instance_writable(true);
        runtime().block_on(search_results(&app, kind));
        assert_eq!(snapshots(&fixture).len(), before.len() + 1, "{kind}");
        let history = fixture
            .state
            .store()
            .get_setting(&fixture.history_key)
            .unwrap()
            .unwrap();
        let history: Value = serde_json::from_str(&history).unwrap();
        assert_eq!(history[0]["query"], "matrix", "{kind}");
    }
}

#[test]
fn readonly_search_keeps_results_and_skips_history() {
    assert_protected_search(true);
}

#[test]
fn maintenance_search_keeps_results_and_skips_history() {
    assert_protected_search(false);
}
