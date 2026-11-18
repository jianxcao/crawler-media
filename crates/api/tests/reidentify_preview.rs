#[path = "reidentify_preview/support.rs"]
mod support;

use domain::MediaKind;
use serde_json::json;
use support::{Fixture, hit};

#[test]
fn same_name_and_year_preview_returns_distinct_candidates_without_changing_identity() {
    let fixture = Fixture::new(Ok(vec![
        hit("101", MediaKind::Movie, Some(2024)),
        hit("202", MediaKind::Movie, Some(2024)),
        hit("101", MediaKind::Movie, Some(2024)),
        hit("303", MediaKind::Tv, Some(2024)),
        hit("404", MediaKind::Movie, Some(2023)),
        hit("505", MediaKind::Movie, None),
    ]));
    let before = fixture
        .fix
        .state
        .store()
        .media_revision(fixture.media.id)
        .unwrap();
    let preview = fixture.preview(fixture.media.id);
    let outcome = &preview["groups"][0]["outcome"];
    assert_eq!(outcome["code"], "ambiguous");
    let candidates = outcome["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0]["tmdb_id"], 101);
    assert_eq!(candidates[1]["tmdb_id"], 202);
    assert!(outcome["media_item_id"].is_null());
    assert!(
        outcome["tmdb_id"].is_null(),
        "preview must not choose the first identity"
    );
    assert_eq!(preview["unreachable"], false);
    let store = fixture.fix.state.store();
    assert_eq!(
        store.get_media(fixture.media.id).unwrap(),
        Some(fixture.media.clone())
    );
    assert_eq!(store.media_revision(fixture.media.id).unwrap(), before);
    assert_eq!(
        store
            .file_coverage_by_id(fixture.file.file_id)
            .unwrap()
            .unwrap()
            .file
            .version,
        fixture.file.version
    );
}

#[test]
fn preview_distinguishes_no_match_from_an_unreachable_catalog_without_mutation() {
    for (result, code, unreachable) in [
        (Ok(Vec::new()), "no_match", false),
        (Err("offline".into()), "tmdb_unreachable", true),
    ] {
        let fixture = Fixture::new(result);
        let preview = fixture.preview(fixture.media.id);
        let outcome = &preview["groups"][0]["outcome"];
        assert_eq!(outcome["code"], code);
        assert_eq!(preview["unreachable"], unreachable);
        assert_eq!(outcome["candidates"], json!([]));
        assert_eq!(
            fixture
                .fix
                .state
                .store()
                .get_media(fixture.media.id)
                .unwrap(),
            Some(fixture.media.clone())
        );
    }
}

#[test]
fn choosing_second_candidate_persists_its_id_and_confirmed_preview_keeps_it_offline() {
    let fixture = Fixture::new(Ok(vec![
        hit("101", MediaKind::Movie, Some(2024)),
        hit("202", MediaKind::Movie, Some(2024)),
    ]));
    let preview = fixture.preview(fixture.media.id);
    let id = preview["groups"][0]["outcome"]["candidates"][1]["tmdb_id"]
        .as_u64()
        .unwrap();
    fixture.bind(&preview, id);
    let store = fixture.fix.state.store();
    let row = store
        .file_coverage_by_id(fixture.file.file_id)
        .unwrap()
        .unwrap();
    let bound = store.get_media(row.file.media_id).unwrap().unwrap();
    assert_eq!(bound.tmdb_id.as_deref(), Some("202"));
    assert_eq!(row.file.version, fixture.file.version);
    *fixture.catalog.result.lock() = Err("offline after manual confirmation".into());
    let confirmed = fixture.preview(bound.id);
    assert_eq!(confirmed["current"]["tmdb_id"], "202");
    assert_eq!(confirmed["groups"][0]["outcome"]["tmdb_id"], "202");
    assert_eq!(confirmed["groups"][0]["outcome"]["same_as_current"], true);
    assert_eq!(confirmed["groups"][0]["outcome"]["candidates"], json!([]));
    assert_eq!(confirmed["unreachable"], false);
    assert_eq!(store.get_media(bound.id).unwrap(), Some(bound));
}

#[test]
fn preview_bounds_candidates_and_never_automatically_binds_a_single_candidate() {
    let fixture = Fixture::new(Ok((1..=30)
        .map(|id| hit(&id.to_string(), MediaKind::Movie, Some(2024)))
        .collect()));
    let preview = fixture.preview(fixture.media.id);
    assert_eq!(
        preview["groups"][0]["outcome"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        20
    );
    *fixture.catalog.result.lock() = Ok(vec![hit("202", MediaKind::Movie, Some(2024))]);
    let single = fixture.preview(fixture.media.id);
    assert_eq!(
        single["groups"][0]["outcome"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(single["groups"][0]["outcome"]["tmdb_id"].is_null());
    assert_eq!(
        fixture
            .fix
            .state
            .store()
            .get_media(fixture.media.id)
            .unwrap(),
        Some(fixture.media.clone())
    );
}
