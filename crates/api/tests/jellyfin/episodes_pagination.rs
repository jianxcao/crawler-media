use super::*;

struct Fixture {
    _tmp: tempfile::TempDir,
    app: axum::Router,
    token: String,
    series: String,
    season_one: String,
    season_two: String,
    other_season: String,
    hidden_series: String,
    hidden_library: String,
    expected: Vec<String>,
    restricted_units: [String; 2],
}

fn library(store: &Store, root: &Path, access_mode: &str) -> String {
    std::fs::create_dir_all(root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Episode pagination fixture",
            &[root.to_str().unwrap()],
            access_mode,
            true,
            &[],
        )
        .unwrap()
        .id
        .to_string()
}

fn media(store: &Store, title: &str) -> MediaId {
    let media = catalog::new_media(MediaKind::Tv, title);
    store.insert_media(&media).unwrap();
    media.id
}

fn seed_episodes(
    store: &Store,
    root: &Path,
    media: MediaId,
    season: u32,
    episodes: std::ops::RangeInclusive<u32>,
) {
    std::fs::create_dir_all(root).unwrap();
    for episode in episodes.rev() {
        catalog::insert_episode(store, root, media, season, episode);
    }
}

async fn member_token(app: &axum::Router) -> String {
    let created = app
        .clone()
        .oneshot(
            Request::post("/api/v1/users")
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"login": "episodes-member", "password": "member-pass"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let login = app
        .clone()
        .oneshot(
            Request::post("/Users/AuthenticateByName")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"Username": "episodes-member", "Pw": "member-pass"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    json(login).await["AccessToken"]
        .as_str()
        .unwrap()
        .to_owned()
}

impl Fixture {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let (app, _, _) = app(tmp.path());
        let store = Store::open(tmp.path().join("data")).unwrap();
        let visible = tmp.path().join("episodes-visible");
        let hidden = tmp.path().join("episodes-hidden");
        library(&store, &visible, "everyone");
        let hidden_library = library(&store, &hidden, "selected");
        let target = media(&store, "Long episode Media");
        let other = media(&store, "Other visible Media");
        let restricted = media(&store, "Restricted Media");
        seed_episodes(&store, &visible.join("target"), target, 2, 1..=20);
        seed_episodes(&store, &visible.join("target"), target, 1, 1..=210);
        seed_episodes(&store, &hidden.join("target"), target, 1, 211..=211);
        seed_episodes(&store, &hidden.join("target"), target, 2, 21..=21);
        seed_episodes(&store, &visible.join("other"), other, 1, 1..=1);
        seed_episodes(&store, &hidden.join("restricted"), restricted, 1, 1..=2);
        let expected = (1..=210)
            .map(|episode| unit_id(target, 1, episode))
            .chain((1..=20).map(|episode| unit_id(target, 2, episode)))
            .collect();
        let token = member_token(&app).await;
        Self {
            _tmp: tmp,
            app,
            token,
            series: work_id(target),
            season_one: season_item_id(target, 1),
            season_two: season_item_id(target, 2),
            other_season: season_item_id(other, 1),
            hidden_series: work_id(restricted),
            hidden_library,
            expected,
            restricted_units: [unit_id(target, 1, 211), unit_id(target, 2, 21)],
        }
    }
}

async fn episodes(f: &Fixture, prefix: &str, token: &str, series: &str, query: &str) -> Value {
    let mut uri = format!("{prefix}/Shows/{series}/Episodes");
    if !query.is_empty() {
        uri.push('?');
        uri.push_str(query);
    }
    let response = f
        .app
        .clone()
        .oneshot(
            Request::get(&uri)
                .header("X-Emby-Token", token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    json(response).await
}

fn assert_page(page: &Value, total: usize, expected: &[String]) {
    assert_eq!(page["TotalRecordCount"], total);
    let ids: Vec<_> = page["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["Id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, expected);
}

#[tokio::test]
async fn episodes_without_limit_returns_all_visible_units_above_two_hundred() {
    let f = Fixture::new().await;
    for prefix in ["", "/emby"] {
        let page = episodes(&f, prefix, &f.token, &f.series, "").await;
        assert_page(&page, 230, &f.expected);
        for item in page["Items"].as_array().unwrap() {
            assert_eq!(item["Type"], "Episode");
            assert_eq!(item["SeriesId"], f.series);
        }
    }
}

#[tokio::test]
async fn episodes_start_index_without_limit_returns_all_remaining_units() {
    let f = Fixture::new().await;
    for prefix in ["", "/emby"] {
        for (offset, expected) in [
            (0, &f.expected[..]),
            (5, &f.expected[5..]),
            (209, &f.expected[209..]),
            (230, &f.expected[230..]),
            (231, &f.expected[230..]),
        ] {
            let query = format!("StartIndex={offset}");
            let page = episodes(&f, prefix, &f.token, &f.series, &query).await;
            assert_page(&page, 230, expected);
        }
    }
}

#[tokio::test]
async fn episodes_explicit_paging_honors_limit_offset_and_total() {
    let f = Fixture::new().await;
    for prefix in ["", "/emby"] {
        for (query, expected) in [
            ("Limit=205", &f.expected[..205]),
            ("StartIndex=105&Limit=3", &f.expected[105..108]),
            ("StartIndex=209&Limit=3", &f.expected[209..212]),
            ("StartIndex=225&Limit=20", &f.expected[225..]),
            ("StartIndex=230&Limit=20", &f.expected[230..]),
        ] {
            let page = episodes(&f, prefix, &f.token, &f.series, query).await;
            assert_page(&page, 230, expected);
        }
    }
}

#[tokio::test]
async fn episodes_season_scope_applies_before_unbounded_and_explicit_paging() {
    let f = Fixture::new().await;
    for prefix in ["", "/emby"] {
        let cases = [
            (
                format!("SeasonId={}", f.season_one),
                210,
                &f.expected[..210],
            ),
            (format!("SeasonId={}", f.season_two), 20, &f.expected[210..]),
            (
                format!("SeasonId={}&StartIndex=5", f.season_one),
                210,
                &f.expected[5..210],
            ),
            (
                format!("SeasonId={}&StartIndex=205&Limit=3", f.season_one),
                210,
                &f.expected[205..208],
            ),
            (
                format!("SeasonId={}", f.other_season),
                0,
                &f.expected[230..],
            ),
        ];
        for (query, total, expected) in cases {
            let page = episodes(&f, prefix, &f.token, &f.series, &query).await;
            assert_page(&page, total, expected);
        }
    }
}

#[tokio::test]
async fn episodes_unbounded_listing_keeps_hidden_and_unknown_scope_isolated() {
    let f = Fixture::new().await;
    let unknown_series = work_id(MediaId::new());
    for prefix in ["", "/emby"] {
        let query = "UserId=00000000-0000-0000-0000-000000000001";
        let visible = episodes(&f, prefix, &f.token, &f.series, query).await;
        assert_page(&visible, 230, &f.expected);
        for series in [&f.hidden_series, &f.hidden_library, &unknown_series] {
            let page = episodes(&f, prefix, &f.token, series, query).await;
            assert_page(&page, 0, &[]);
        }
        let admin = episodes(&f, prefix, "admin-token", &f.hidden_series, "").await;
        assert_eq!(admin["TotalRecordCount"], 2);
        assert_eq!(admin["Items"].as_array().unwrap().len(), 2);
        let mut admin_expected = f.expected.clone();
        admin_expected.insert(210, f.restricted_units[0].clone());
        admin_expected.push(f.restricted_units[1].clone());
        let admin = episodes(&f, prefix, "admin-token", &f.series, "").await;
        assert_page(&admin, 232, &admin_expected);
    }
}
