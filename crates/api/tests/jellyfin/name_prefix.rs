use super::*;

#[path = "name_prefix_episode.rs"]
mod episode;

struct Fixture {
    _tmp: tempfile::TempDir,
    app: axum::Router,
    library: String,
    hidden: String,
    token: String,
    alpha: Vec<String>,
    series: String,
    literal: String,
}

fn library(store: &Store, root: &Path, kind: MediaKind, visibility: &str) -> String {
    std::fs::create_dir_all(root).unwrap();
    store
        .create_library(
            kind,
            "Prefix fixture",
            &[root.to_str().unwrap()],
            visibility,
            true,
            &[],
        )
        .unwrap()
        .id
        .to_string()
}

fn publish(store: &Store, root: &Path, media: &Media, file_name: &str) {
    let path = root.join(file_name);
    std::fs::write(&path, b"media").unwrap();
    store::test_support::publish_fixture_file(
        store,
        &FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            season: (media.kind == MediaKind::Tv).then_some(1),
            episode: (media.kind == MediaKind::Tv).then_some(1),
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            ..Default::default()
        },
    )
    .unwrap();
}

fn media(store: &Store, kind: MediaKind, title: &str) -> Media {
    let media = catalog::new_media(kind, title);
    store.insert_media(&media).unwrap();
    media
}

fn adopt_search_text(store: &Store, media: MediaId) {
    store
        .adopt_metadata(
            &domain::MetadataAdoption {
                media_id: media,
                scope: domain::EntityScope::Work,
                locale: "en-US".into(),
                source: "tmdb".into(),
                provider_entity_id: "prefix".into(),
                provider_work_id: "prefix".into(),
                fetched_at: 1000,
                normalized: domain::NormalizedMetadata {
                    fields: domain::MetadataFields {
                        original_title: domain::FieldValue::Value("OriginalOnly".into()),
                        overview: domain::FieldValue::Value("OverviewOnly".into()),
                        ..Default::default()
                    },
                    field_origins: std::collections::BTreeMap::new(),
                },
                expected_revision: Some(0),
                preferences: domain::MetadataPreferences {
                    language_priority: vec!["en-US".into()],
                    cert_country_priority: vec!["US".into()],
                },
            },
            1000,
        )
        .unwrap();
}

async fn member_token(app: &axum::Router) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::post("/api/v1/users")
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"login":"prefix-member","password":"prefix-pass"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let response = app
        .clone()
        .oneshot(
            Request::post("/Users/AuthenticateByName")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"Username":"prefix-member","Pw":"prefix-pass"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json(response).await["AccessToken"]
        .as_str()
        .unwrap()
        .to_string()
}

impl Fixture {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let (app, _, _) = app(tmp.path());
        let store = Store::open(tmp.path().join("data")).unwrap();
        let visible = tmp.path().join("prefix-visible");
        let hidden_root = tmp.path().join("prefix-hidden");
        let duplicate_root = tmp.path().join("prefix-duplicate");
        let tv = tmp.path().join("prefix-tv");
        let visible_library = library(&store, &visible, MediaKind::Movie, "everyone");
        let hidden = library(&store, &hidden_root, MediaKind::Movie, "selected");
        library(&store, &duplicate_root, MediaKind::Movie, "everyone");
        library(&store, &tv, MediaKind::Tv, "everyone");
        let mut alpha = Vec::new();
        for index in 0..30 {
            let name = if index < 3 {
                format!("Alpha {index:02}")
            } else {
                format!("Movie {index:02}")
            };
            let item = media(&store, MediaKind::Movie, &name);
            publish(&store, &visible, &item, &format!("{index}.mkv"));
            if index < 3 {
                alpha.push(work_id(item.id));
            }
            if index == 0 {
                publish(&store, &hidden_root, &item, "duplicate.mkv");
                publish(&store, &duplicate_root, &item, "duplicate.mkv");
                adopt_search_text(&store, item.id);
            }
        }
        let secret = media(&store, MediaKind::Movie, "Alpha Secret");
        publish(&store, &hidden_root, &secret, "secret.mkv");
        let literal = media(&store, MediaKind::Movie, "Percent%_Literal");
        publish(&store, &hidden_root, &literal, "literal.mkv");
        let show = media(&store, MediaKind::Tv, "Alpha Series");
        publish(&store, &tv, &show, "episode.mkv");
        let token = member_token(&app).await;
        Self {
            _tmp: tmp,
            app,
            library: visible_library,
            hidden,
            token,
            alpha,
            series: work_id(show.id),
            literal: work_id(literal.id),
        }
    }
}

async fn query(f: &Fixture, prefix: &str, token: &str, query: &str) -> Value {
    let uri = format!("{prefix}/Items?{query}");
    let response = f
        .app
        .clone()
        .oneshot(
            Request::get(&uri)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    json(response).await
}

fn ids(page: &Value) -> Vec<String> {
    page["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["Id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn name_prefix_filters_items_before_count_and_pagination() {
    let f = Fixture::new().await;
    for prefix in ["", "/emby"] {
        let base = format!(
            "ParentId={}&Recursive=true&IncludeItemTypes=Movie",
            f.library
        );
        let all = query(&f, prefix, &f.token, &base).await;
        assert_eq!(all["TotalRecordCount"], 30);
        let absent = query(&f, prefix, &f.token, &format!("{base}&NameStartsWith=none")).await;
        assert_eq!(absent["TotalRecordCount"], 0);
        assert!(ids(&absent).is_empty());
        let mut collected = Vec::new();
        for offset in [0, 2] {
            let page = query(&f, prefix, &f.token, &format!("{base}&nameStartsWith=aLpHa&SortBy=SortName&SortOrder=Ascending&StartIndex={offset}&Limit=2")).await;
            assert_eq!(page["TotalRecordCount"], 3);
            collected.extend(ids(&page));
        }
        assert_eq!(collected, f.alpha);
        let interior = query(&f, prefix, &f.token, &format!("{base}&NameStartsWith=pha")).await;
        assert_eq!(interior["TotalRecordCount"], 0);
        assert!(ids(&interior).is_empty());
        for term in ["OriginalOnly", "OverviewOnly"] {
            let search = query(&f, prefix, &f.token, &format!("{base}&SearchTerm={term}")).await;
            assert_eq!(ids(&search), vec![f.alpha[0].clone()]);
            let filtered = query(
                &f,
                prefix,
                &f.token,
                &format!("{base}&NameStartsWith={term}"),
            )
            .await;
            assert_eq!(filtered["TotalRecordCount"], 0);
            assert!(ids(&filtered).is_empty());
        }
    }
}

#[tokio::test]
async fn name_prefix_respects_series_visibility_and_parent() {
    let f = Fixture::new().await;
    for prefix in ["", "/emby"] {
        let page = query(
            &f,
            prefix,
            &f.token,
            "Recursive=true&IncludeItemTypes=Series&NameStartsWith=alpha",
        )
        .await;
        assert_eq!(page["TotalRecordCount"], 1);
        assert_eq!(ids(&page), vec![f.series.clone()]);
        let hidden = query(
            &f,
            prefix,
            &f.token,
            &format!("ParentId={}&Recursive=true&NameStartsWith=alpha", f.hidden),
        )
        .await;
        assert_eq!(hidden["TotalRecordCount"], 0);
        assert!(ids(&hidden).is_empty());
        let unknown = query(
            &f,
            prefix,
            &f.token,
            "ParentId=ffffffffffffffffffffffffffffffff&Recursive=true&NameStartsWith=alpha",
        )
        .await;
        assert_eq!(unknown["TotalRecordCount"], 0);
        assert!(ids(&unknown).is_empty());
        let all = query(
            &f,
            prefix,
            &f.token,
            "Recursive=true&IncludeItemTypes=Movie&NameStartsWith=alpha",
        )
        .await;
        assert_eq!(all["TotalRecordCount"], 3);
        let mut got = ids(&all);
        got.sort();
        let mut expected = f.alpha.clone();
        expected.sort();
        assert_eq!(got, expected);
        let literal = query(
            &f,
            prefix,
            "admin-token",
            &format!(
                "ParentId={}&Recursive=true&NameStartsWith=Percent%25_",
                f.hidden
            ),
        )
        .await;
        assert_eq!(literal["TotalRecordCount"], 1);
        assert_eq!(ids(&literal), vec![f.literal.clone()]);
        let wildcard = query(
            &f,
            prefix,
            "admin-token",
            &format!(
                "ParentId={}&Recursive=true&NameStartsWith=Percent%25Literal",
                f.hidden
            ),
        )
        .await;
        assert_eq!(wildcard["TotalRecordCount"], 0);
    }
}
