use super::*;

fn adopt_episode(store: &Store, media_id: MediaId) {
    store
        .adopt_metadata(
            &domain::MetadataAdoption {
                media_id,
                scope: domain::EntityScope::Work,
                locale: "zh-CN".into(),
                source: "tmdb".into(),
                provider_entity_id: "prefix-series".into(),
                provider_work_id: "prefix-series".into(),
                fetched_at: 1000,
                normalized: domain::NormalizedMetadata {
                    fields: domain::MetadataFields::default(),
                    field_origins: std::collections::BTreeMap::new(),
                },
                expected_revision: Some(0),
                preferences: domain::MetadataPreferences {
                    language_priority: vec!["zh-CN".into()],
                    cert_country_priority: vec!["US".into()],
                },
            },
            1000,
        )
        .unwrap();
    store
        .adopt_metadata(
            &domain::MetadataAdoption {
                media_id,
                scope: domain::EntityScope::Episode {
                    season: 1,
                    episode: 2,
                },
                locale: "zh-CN".into(),
                source: "tmdb".into(),
                provider_entity_id: "episode-two".into(),
                provider_work_id: "prefix-series".into(),
                fetched_at: 1000,
                normalized: domain::NormalizedMetadata {
                    fields: domain::MetadataFields {
                        title: domain::FieldValue::Value("Catalog Episode".into()),
                        ..Default::default()
                    },
                    field_origins: std::collections::BTreeMap::new(),
                },
                expected_revision: Some(0),
                preferences: domain::MetadataPreferences {
                    language_priority: vec!["zh-CN".into()],
                    cert_country_priority: vec!["US".into()],
                },
            },
            1000,
        )
        .unwrap();
}

fn insert_episode(store: &Store, root: &Path, media_id: MediaId, episode: u32) -> String {
    catalog::insert_episode(store, root, media_id, 1, episode)
}

async fn items(app: &axum::Router, prefix: &str, series: &str, name: Option<&str>) -> Value {
    let filter = name
        .map(|value| format!("&NameStartsWith={value}"))
        .unwrap_or_default();
    let uri = format!("{prefix}/Items?SeriesId={series}{filter}");
    let response = app
        .clone()
        .oneshot(
            Request::get(&uri)
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    json(response).await
}

#[tokio::test]
async fn name_prefix_episode_uses_each_display_name_and_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("episodes");
    let named = root.join("named");
    let unnamed = root.join("unnamed");
    std::fs::create_dir_all(&named).unwrap();
    std::fs::create_dir_all(&unnamed).unwrap();
    library(&store, &root, MediaKind::Tv, "everyone");
    let show = media(&store, MediaKind::Tv, "WorkFallback");
    std::fs::write(
        named.join("tvshow.nfo"),
        "<tvshow><title>WorkFallback</title></tvshow>",
    )
    .unwrap();
    std::fs::write(named.join("Show S01E01.nfo"), "<episodedetails><title>Nfo Episode</title><season>1</season><episode>1</episode></episodedetails>").unwrap();
    let nfo = insert_episode(&store, &named, show.id, 1);
    let adopted = insert_episode(&store, &named, show.id, 2);
    let fallback = insert_episode(&store, &named, show.id, 3);
    adopt_episode(&store, show.id);
    let plain = media(&store, MediaKind::Tv, "No metadata series");
    let unnamed_id = insert_episode(&store, &unnamed, plain.id, 1);
    for prefix in ["", "/emby"] {
        let series = work_id(show.id);
        let all = items(&app, prefix, &series, None).await;
        let names: Vec<_> = all["Items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| (item["Id"].as_str().unwrap(), item["Name"].as_str().unwrap()))
            .collect();
        assert!(names.contains(&(nfo.as_str(), "Nfo Episode")));
        assert!(names.contains(&(adopted.as_str(), "Catalog Episode")));
        assert!(names.contains(&(fallback.as_str(), "WorkFallback")));
        for (term, expected) in [
            ("Nfo", &nfo),
            ("Catalog", &adopted),
            ("WorkFallback", &fallback),
        ] {
            let page = items(&app, prefix, &series, Some(term)).await;
            assert_eq!(page["TotalRecordCount"], 1, "{term}");
            assert_eq!(ids(&page), vec![expected.clone()], "{term}");
        }
        let page = items(&app, prefix, &work_id(plain.id), None).await;
        assert_eq!(page["Items"][0]["Name"], "S01E01");
        let page = items(&app, prefix, &work_id(plain.id), Some("S01E01")).await;
        assert_eq!(page["TotalRecordCount"], 1);
        assert_eq!(ids(&page), vec![unnamed_id.clone()]);
        let page = items(&app, prefix, &work_id(plain.id), Some("No%20metadata")).await;
        assert_eq!(page["TotalRecordCount"], 0);
    }
}

#[tokio::test]
async fn name_prefix_episode_explicit_empty_blocks_work_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("empty-episode");
    library(&store, &root, MediaKind::Tv, "everyone");
    let show = media(&store, MediaKind::Tv, "WorkFallback");
    let episode = insert_episode(&store, &root, show.id, 2);
    adopt_episode(&store, show.id);
    store
        .set_metadata_overrides(
            show.id,
            domain::EntityScope::Episode {
                season: 1,
                episode: 2,
            },
            "zh-CN",
            &domain::MetadataFields {
                title: domain::FieldValue::Empty,
                ..Default::default()
            },
            0,
        )
        .unwrap();
    let series = work_id(show.id);
    let all = items(&app, "", &series, None).await;
    assert_eq!(all["Items"][0]["Name"], "S01E02");
    let fallback = items(&app, "", &series, Some("S01E02")).await;
    assert_eq!(ids(&fallback), vec![episode]);
    assert_eq!(
        items(&app, "", &series, Some("Catalog")).await["TotalRecordCount"],
        0
    );
    assert_eq!(
        items(&app, "", &series, Some("WorkFallback")).await["TotalRecordCount"],
        0
    );
}

#[tokio::test]
async fn name_prefix_filters_each_library_before_deduplicating_episode() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let first = tmp.path().join("first");
    let second = tmp.path().join("second");
    library(&store, &first, MediaKind::Tv, "everyone");
    library(&store, &second, MediaKind::Tv, "everyone");
    let show = media(&store, MediaKind::Tv, "Shared series");
    for (root, name) in [(&first, "First Library"), (&second, "Second Library")] {
        std::fs::write(root.join("Show S01E01.nfo"), format!("<episodedetails><title>{name}</title><season>1</season><episode>1</episode></episodedetails>")).unwrap();
        insert_episode(&store, root, show.id, 1);
    }
    let series = work_id(show.id);
    for name in ["First", "Second"] {
        let page = items(&app, "", &series, Some(name)).await;
        assert_eq!(page["TotalRecordCount"], 1, "{name}");
        assert_eq!(page["Items"][0]["Name"], format!("{name} Library"));
    }
}

#[tokio::test]
async fn name_prefix_keeps_selection_and_hydrated_name_in_same_snapshot() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let tmp = tempfile::tempdir().unwrap();
    let _ = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("snapshot-episode");
    library(&store, &root, MediaKind::Tv, "everyone");
    let show = media(&store, MediaKind::Tv, "Snapshot series");
    insert_episode(&store, &root, show.id, 2);
    adopt_episode(&store, show.id);
    let fired = Arc::new(AtomicBool::new(false));
    let observed = fired.clone();
    let writer = Store::open(tmp.path().join("data")).unwrap();
    let media_id = show.id;
    let state = ApiState::new(
        store,
        "admin-token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .unwrap()
    .with_library_read_stage_barrier(Arc::new(move |stage| {
        if stage == "hydrate" && !observed.swap(true, Ordering::SeqCst) {
            writer
                .set_metadata_overrides(
                    media_id,
                    domain::EntityScope::Episode {
                        season: 1,
                        episode: 2,
                    },
                    "zh-CN",
                    &domain::MetadataFields {
                        title: domain::FieldValue::Value("Changed Episode".into()),
                        ..Default::default()
                    },
                    0,
                )
                .unwrap();
        }
    }));
    let app = router(state);
    let page = items(&app, "", &work_id(show.id), Some("Catalog")).await;
    assert!(fired.load(Ordering::SeqCst));
    assert_eq!(page["TotalRecordCount"], 1);
    assert_eq!(page["Items"][0]["Name"], "Catalog Episode");
}
