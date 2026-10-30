//! Public continuation contracts; no SQL text or cross-thread fake measurements.
use api::{ApiState, Store};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use domain::{
    Coverage, FetchMode, Filter, FilterId, Media, MediaId, MediaKind, Subscribe, SubscribeId,
    TypedItemId, UnitKey, User, UserId, UserRole,
};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::{
    provider::QueryTiming,
    query_metrics::{QueryMetrics, QueryMetricsSnapshot},
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::Ordering};
use tower::ServiceExt;

const TOKEN: &str = "continuation-fixture";
const PEER_TOKEN: &str = "continuation-peer";
struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}
struct Fixture {
    dir: tempfile::TempDir,
    state: ApiState,
    user: UserId,
}
impl Fixture {
    fn new() -> Self {
        std::fs::create_dir_all("/tmp/crawler-storage-fixes").unwrap();
        let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let state = ApiState::new(
            store,
            TOKEN.into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap();
        let user = state.store().admin_user_id().unwrap();
        Self { dir, state, user }
    }
    fn store(&self) -> Arc<Store> {
        self.state.store()
    }
    fn library(&self, name: &str, kind: MediaKind, visible: bool, home: bool) -> (String, PathBuf) {
        let root = self.dir.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        let library = self
            .store()
            .create_library(
                kind,
                name,
                &[root.to_str().unwrap()],
                "everyone",
                visible,
                &[],
            )
            .unwrap();
        self.store()
            .set_library_switch_settings(&library.id, None, None, None, Some(!home), None)
            .unwrap();
        (library.id, root)
    }
    fn media(&self, kind: MediaKind, title: &str) -> MediaId {
        let media = Media {
            id: MediaId::new(),
            kind,
            title: title.into(),
            year: Some(2024),
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        self.store().insert_media(&media).unwrap();
        media.id
    }
    fn publish(
        &self,
        root: &Path,
        media: MediaId,
        coordinate: Option<(u32, u32)>,
        version: &str,
    ) -> domain::LibraryFile {
        let stem = coordinate
            .map(|(s, e)| format!("S{s:02}E{e:02}-{version}"))
            .unwrap_or_else(|| version.into());
        let path = root.join(format!("{media}-{stem}.mkv"));
        std::fs::write(&path, b"finite video").unwrap();
        if let Some((s, e)) = coordinate {
            std::fs::write(
                path.with_extension("nfo"),
                format!("<episodedetails><title>Local S{s}E{e}</title></episodedetails>"),
            )
            .unwrap();
        }
        store::test_support::publish_fixture_file(
            &self.store(),
            &store::test_support::FixtureFile {
                media_id: media,
                path: path.display().to_string(),
                season: coordinate.map(|c| c.0),
                episode: coordinate.map(|c| c.1),
                ..Default::default()
            },
        )
        .unwrap()
    }
    fn state(&self, media: MediaId, coordinate: (i32, i32), position: i64, played: bool, now: i64) {
        self.store()
            .upsert_unit(
                self.user,
                media,
                coordinate.0,
                coordinate.1,
                position,
                Some(played),
                None,
                Some(120_000),
                None,
                None,
                false,
                now,
            )
            .unwrap();
    }
    fn subscribe(
        &self,
        user: UserId,
        media: MediaId,
        library: &str,
        season: u32,
        from: u32,
        to: u32,
    ) {
        let filter = Filter::new(FilterId::new(), "Continuation filter", vec![]);
        self.store().insert_filter(&filter).unwrap();
        self.store()
            .insert_subscribe(&Subscribe {
                id: SubscribeId::new(),
                user_id: user,
                media_id: media,
                coverage: Coverage::Tv {
                    season,
                    episode_from: from,
                    episode_to: Some(to),
                },
                fetch_mode: FetchMode::Search,
                filter_id: filter.id,
                wash_cut: false,
                wash_cut_filter_id: None,
                keep_old_versions: false,
                full_season_pack: false,
                downloader_id: None,
                library_id: Some(library.parse().unwrap()),
                tracking_state: "active".into(),
                follow_future: false,
                search_interval_secs: 1800,
            })
            .unwrap();
    }
    fn peer(&self) -> UserId {
        let user = User {
            id: UserId::new(),
            login: "continuation-peer".into(),
            enabled: true,
            role: UserRole::Member,
        };
        self.store().insert_user(&user).unwrap();
        self.store().set_user_token(user.id, PEER_TOKEN).unwrap();
        user.id
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn unit(media: MediaId, s: i32, e: i32) -> String {
    TypedItemId::Unit {
        media,
        key: UnitKey::new(s, e),
    }
    .to_wire_string()
}
fn work(media: MediaId) -> String {
    TypedItemId::Work(media).to_wire_string()
}
async fn get(state: &ApiState, uri: &str, token: &str) -> (Value, QueryMetricsSnapshot) {
    let timing = QueryTiming::start();
    let metrics = QueryMetrics::new(timing, true);
    let mut req = Request::builder()
        .uri(uri)
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(timing);
    req.extensions_mut().insert(metrics.clone());
    let response = api::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    let body = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (serde_json::from_slice(&body).unwrap(), metrics.snapshot())
}
fn items(body: &Value) -> &[Value] {
    body["Items"].as_array().unwrap()
}
fn home(body: &Value) -> &[Value] {
    body["data"]["items"].as_array().unwrap()
}
fn home_row(body: &Value, media: MediaId) -> &Value {
    home(body)
        .iter()
        .find(|row| row["media_item_id"] == media.to_string())
        .unwrap()
}

#[test]
fn jellyfin_nextup_preserves_unstarted_cross_library_and_total_json() {
    let fx = Fixture::new();
    let (a, ar) = fx.library("earlier-season", MediaKind::Tv, true, true);
    let (b, br) = fx.library("later-season", MediaKind::Tv, true, true);
    let (_, cr) = fx.library("unstarted", MediaKind::Tv, true, true);
    let (_, hr) = fx.library("hidden", MediaKind::Tv, false, true);
    let shared = fx.media(MediaKind::Tv, "Cross season");
    let untouched = fx.media(MediaKind::Tv, "Unstarted");
    let hidden = fx.media(MediaKind::Tv, "Invisible");
    fx.publish(&ar, shared, Some((1, 1)), "one");
    fx.publish(&br, shared, Some((2, 1)), "two");
    fx.publish(&br, shared, Some((2, 1)), "duplicate");
    fx.publish(&cr, untouched, Some((1, 1)), "one");
    fx.publish(&hr, hidden, Some((1, 1)), "one");
    fx.state(shared, (1, 1), 0, true, 30);
    runtime().block_on(async {
        let (all, _) = get(&fx.state, "/Shows/NextUp", TOKEN).await;
        assert_eq!(all["TotalRecordCount"], 2);
        assert_eq!(
            items(&all)
                .iter()
                .filter(|item| item["Id"] == unit(shared, 2, 1))
                .count(),
            1
        );
        assert!(
            items(&all)
                .iter()
                .any(|item| item["Id"] == unit(untouched, 1, 1))
        );
        let (pinned, _) = get(
            &fx.state,
            &format!("/Shows/NextUp?SeriesId={}&Limit=1", work(shared)),
            TOKEN,
        )
        .await;
        assert_eq!(pinned["TotalRecordCount"], 1);
        assert_eq!(items(&pinned)[0]["Id"], unit(shared, 2, 1));
        assert_eq!(items(&pinned)[0]["SeriesName"], "Cross season");
        assert_eq!(items(&pinned)[0]["UserData"]["Played"], false);
        fx.store().begin_library_removal(&b).unwrap();
        let (after, _) = get(&fx.state, "/Shows/NextUp", TOKEN).await;
        assert_eq!(after["TotalRecordCount"], 1);
        assert_eq!(items(&after)[0]["Id"], unit(untouched, 1, 1));
        assert_ne!(a, b);
    });
}

#[test]
fn next_up_orders_by_activity_and_dedupes_before_paging() {
    let fx = Fixture::new();
    let (_, root) = fx.library("ordered", MediaKind::Tv, true, true);
    let (_, duplicate) = fx.library("ordered-copy", MediaKind::Tv, true, true);
    let mut expected = Vec::new();
    for (title, clock) in [
        ("Zulu latest", 30),
        ("Alpha older", 20),
        ("Middle older", 20),
    ] {
        let id = fx.media(MediaKind::Tv, title);
        fx.publish(&root, id, Some((1, 1)), "one");
        fx.publish(&root, id, Some((1, 2)), "two");
        fx.state(id, (1, 1), 0, true, clock);
        expected.push((clock, id));
    }
    fx.publish(&duplicate, expected[0].1, Some((1, 2)), "duplicate");
    expected.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.to_string().cmp(&b.1.to_string())));
    runtime().block_on(async {
        let (all, _) = get(&fx.state, "/Shows/NextUp?Limit=10", TOKEN).await;
        let ids: Vec<_> = items(&all)
            .iter()
            .map(|item| item["Id"].as_str().unwrap().to_owned())
            .collect();
        let wanted: Vec<_> = expected.iter().map(|(_, id)| unit(*id, 1, 2)).collect();
        assert_eq!(ids, wanted);
        let (page, _) = get(&fx.state, "/Shows/NextUp?StartIndex=1&Limit=1", TOKEN).await;
        assert_eq!(page["TotalRecordCount"], 3);
        assert_eq!(items(&page)[0]["Id"], wanted[1]);
        let (end, _) = get(&fx.state, "/Shows/NextUp?StartIndex=99&Limit=1", TOKEN).await;
        assert_eq!(end["TotalRecordCount"], 3);
        assert!(items(&end).is_empty());
    });
}

#[test]
fn home_upnext_keeps_activity_anchor_subscription_window_and_movie_contracts() {
    let fx = Fixture::new();
    let (library, root) = fx.library("home", MediaKind::Tv, true, true);
    let (_, movie_root) = fx.library("movies", MediaKind::Movie, true, true);
    let anchor = fx.media(MediaKind::Tv, "Activity anchored");
    for coordinate in [(1, 1), (1, 3), (2, 1)] {
        fx.publish(&root, anchor, Some(coordinate), "one");
    }
    fx.state(anchor, (1, 3), 0, true, 40);
    let subscribed = fx.media(MediaKind::Tv, "Subscribed window");
    for coordinate in [(1, 1), (2, 1), (2, 2), (2, 3)] {
        fx.publish(&root, subscribed, Some(coordinate), "one");
    }
    fx.subscribe(fx.user, subscribed, &library, 2, 2, 2);
    let untouched = fx.media(MediaKind::Tv, "Unsubscribed untouched");
    fx.publish(&root, untouched, Some((1, 1)), "one");
    let movie = fx.media(MediaKind::Movie, "Partial movie");
    fx.publish(&movie_root, movie, None, "one");
    fx.state(movie, (-1, -1), 50_000, false, 50);
    runtime().block_on(async {
        let (body, _) = get(&fx.state, "/api/v1/playback/up-next?limit=20", TOKEN).await;
        assert_eq!(home(&body).len(), 3);
        let row = home_row(&body, anchor);
        assert_eq!(row["season_number"], 2);
        assert_eq!(row["episode_number"], 1);
        assert_eq!(row["advanced"], true);
        assert_eq!(row["episode_title"], "Local S2E1");
        let row = home_row(&body, subscribed);
        assert_eq!(row["season_number"], 2);
        assert_eq!(row["episode_number"], 2);
        assert_eq!(row["advanced"], false);
        let row = home_row(&body, movie);
        assert_eq!(row["season_number"], 0);
        assert_eq!(row["episode_number"], 0);
        assert_eq!(row["position_ms"], 50_000);
        assert_eq!(row["duration_ms"], 120_000);
        assert_eq!(row["progress_percent"], 42);
        assert_eq!(row["last_played_at"], "50000");
        let (jellyfin, _) = get(
            &fx.state,
            &format!("/Shows/NextUp?SeriesId={}", work(anchor)),
            TOKEN,
        )
        .await;
        assert_eq!(
            items(&jellyfin)[0]["Id"],
            unit(anchor, 1, 1),
            "Jellyfin selects the first unplayed; home respects the activity anchor"
        );
        fx.state(movie, (-1, -1), 0, true, 60);
        let (after, _) = get(&fx.state, "/api/v1/playback/up-next", TOKEN).await;
        assert!(
            !home(&after)
                .iter()
                .any(|item| item["media_item_id"] == movie.to_string())
        );
    });
}

#[test]
fn home_upnext_source_stays_in_eligible_library_and_users_are_isolated() {
    let fx = Fixture::new();
    let peer = fx.peer();
    let (_, excluded) = fx.library("a-excluded", MediaKind::Tv, true, false);
    let (eligible, root) = fx.library("z-eligible", MediaKind::Tv, true, true);
    let (_, hidden) = fx.library("hidden-home", MediaKind::Tv, false, true);
    let shared = fx.media(MediaKind::Tv, "Eligible source");
    fx.publish(&excluded, shared, Some((1, 1)), "excluded");
    fx.publish(&root, shared, Some((1, 2)), "eligible");
    fx.state(shared, (1, 2), 20_000, false, 20);
    let hidden_only = fx.media(MediaKind::Tv, "Only hidden");
    fx.publish(&hidden, hidden_only, Some((1, 1)), "one");
    fx.state(hidden_only, (1, 1), 10_000, false, 30);
    let subscribed = fx.media(MediaKind::Tv, "Admin subscription");
    fx.publish(&root, subscribed, Some((2, 2)), "one");
    fx.subscribe(fx.user, subscribed, &eligible, 2, 2, 2);
    runtime().block_on(async {
        let (admin, _) = get(&fx.state, "/api/v1/playback/up-next", TOKEN).await;
        assert_eq!(home(&admin).len(), 2);
        let row = home_row(&admin, shared);
        assert_eq!(
            row["library_id"], eligible,
            "the source must own the selected episode and remain home-eligible"
        );
        assert_eq!(row["season_number"], 1);
        assert_eq!(row["episode_number"], 2);
        let (other, _) = get(&fx.state, "/api/v1/playback/up-next", PEER_TOKEN).await;
        assert!(
            home(&other).is_empty(),
            "another user's history and Subscribe must not create home rows"
        );
        assert_ne!(fx.user, peer);
    });
}

#[test]
fn resume_parent_scope_preserves_effective_marks_and_selected_unit_source() {
    let fx = Fixture::new();
    let (a, ar) = fx.library("resume-a", MediaKind::Tv, true, true);
    let (b, br) = fx.library("resume-b", MediaKind::Tv, true, true);
    let media = fx.media(MediaKind::Tv, "Cross source resume");
    fx.publish(&ar, media, Some((1, 1)), "one");
    let selected = fx.publish(&br, media, Some((2, 1)), "two");
    fx.state(media, (1, 1), 10_000, false, 10);
    fx.state(media, (2, 1), 20_000, false, 20);
    runtime().block_on(async {
        let route = format!("/Users/{}/Items/Resume", fx.user);
        let (all, _) = get(&fx.state, &route, TOKEN).await;
        assert_eq!(all["TotalRecordCount"], 1);
        assert_eq!(items(&all)[0]["Id"], unit(media, 2, 1));
        assert_eq!(items(&all)[0]["Path"], selected.path.display().to_string());
        assert_eq!(
            items(&all)[0]["UserData"]["PlaybackPositionTicks"],
            200_000_000
        );
        let (scoped, _) = get(&fx.state, &format!("{route}?ParentId={b}&Limit=1"), TOKEN).await;
        assert_eq!(scoped["TotalRecordCount"], 1);
        assert_eq!(items(&scoped)[0]["Id"], unit(media, 2, 1));
        assert_eq!(
            items(&scoped)[0]["Path"],
            selected.path.display().to_string()
        );
        let (early, _) = get(&fx.state, &format!("{route}?ParentId={a}&Limit=1"), TOKEN).await;
        assert_eq!(early["TotalRecordCount"], 1);
        assert_eq!(items(&early)[0]["Id"], unit(media, 1, 1));
        fx.store()
            .upsert_unit(
                fx.user,
                media,
                2,
                -1,
                0,
                Some(true),
                None,
                None,
                None,
                None,
                false,
                30,
            )
            .unwrap();
        let (marked, _) = get(&fx.state, &format!("{route}?ParentId={b}"), TOKEN).await;
        assert_eq!(marked["TotalRecordCount"], 0);
        assert!(items(&marked).is_empty());
        fx.state(media, (2, 1), 25_000, false, 40);
        let (restored, _) = get(&fx.state, &format!("{route}?ParentId={b}"), TOKEN).await;
        assert_eq!(items(&restored)[0]["Id"], unit(media, 2, 1));
    });
}

fn home_work(history: usize) -> (u64, u64) {
    let fx = Fixture::new();
    let (_, root) = fx.library("history", MediaKind::Movie, true, true);
    let target = fx.media(MediaKind::Movie, "Still partial");
    fx.publish(&root, target, None, "one");
    fx.state(target, (-1, -1), 50_000, false, 1);
    for index in 0..history {
        let media = fx.media(MediaKind::Movie, &format!("Completed {index:03}"));
        fx.publish(&root, media, None, "one");
        fx.state(media, (-1, -1), 0, true, 100 + index as i64);
    }
    let (body, stats) =
        runtime().block_on(get(&fx.state, "/api/v1/playback/up-next?limit=20", TOKEN));
    assert_eq!(home(&body).len(), 1);
    assert_eq!(home(&body)[0]["media_item_id"], target.to_string());
    let statements = stats
        .statements
        .expect("real Store worker diagnostics must be present");
    let vm = stats.vm_steps.expect("VM work must be measured");
    assert!(
        statements > 0 && vm > 0,
        "zero cannot prove fixed work across async worker threads"
    );
    (statements, vm)
}

#[test]
fn up_next_statement_count_is_constant() {
    let small = home_work(10);
    let large = home_work(200);
    assert_eq!(
        small.0, large.0,
        "one fixed response page must not query each historical Media: {small:?} vs {large:?}"
    );
    assert!(
        large.1 <= small.1 + 2500,
        "unrelated completed history must not drive candidate VM work: {small:?} vs {large:?}"
    );
}

#[test]
fn jellyfin_nextup_hydrates_only_the_returned_page_after_unrelated_growth() {
    for count in [10, 200] {
        let fx = Fixture::new();
        let (_, root) = fx.library("page", MediaKind::Tv, true, true);
        for index in 0..count {
            let media = fx.media(MediaKind::Tv, &format!("Unstarted {index:03}"));
            fx.publish(&root, media, Some((1, 1)), "one");
        }
        fx.state.jellyfin_heavy_items().store(0, Ordering::SeqCst);
        fx.state.jellyfin_hydrated_rows().store(0, Ordering::SeqCst);
        let (body, _) = runtime().block_on(get(&fx.state, "/Shows/NextUp?Limit=5", TOKEN));
        assert_eq!(body["TotalRecordCount"], count);
        assert_eq!(items(&body).len(), 5);
        assert_eq!(fx.state.jellyfin_heavy_items().load(Ordering::SeqCst), 5);
        assert!(
            fx.state.jellyfin_hydrated_rows().load(Ordering::SeqCst) <= 40,
            "heavy facts must stay within the five selected episode rows"
        );
    }
}

#[test]
fn home_subscription_uses_owned_max_after_declared_to_and_all_eligible_owners() {
    let fx = Fixture::new();
    let (owner, root) = fx.library("subscription-owner", MediaKind::Tv, true, true);
    let (destination, _) = fx.library("subscription-destination", MediaKind::Tv, true, true);
    let media = fx.media(MediaKind::Tv, "Existing subscription range contract");
    fx.publish(&root, media, Some((2, 4)), "owned-after-declared-to");
    fx.subscribe(fx.user, media, &destination, 2, 2, 2);
    runtime().block_on(async {
        let (body, _) = get(&fx.state, "/api/v1/playback/up-next", TOKEN).await;
        let row = home_row(&body, media);
        assert_eq!(row["season_number"], 2);
        assert_eq!(row["episode_number"], 4);
        assert_eq!(row["library_id"], owner);
        assert_eq!(row["advanced"], false);
        assert_eq!(row["unwatched_ahead_count"], 0);
    });
}

#[test]
fn jellyfin_nextup_explicit_series_keeps_unplayed_movie_work() {
    let fx = Fixture::new();
    let (_, root) = fx.library("pinned-movie", MediaKind::Movie, true, true);
    let media = fx.media(MediaKind::Movie, "Explicit movie continuation");
    fx.publish(&root, media, None, "whole");
    runtime().block_on(async {
        let (all, _) = get(&fx.state, "/Shows/NextUp", TOKEN).await;
        assert_eq!(all["TotalRecordCount"], 0);
        let route = format!("/Shows/NextUp?SeriesId={}", work(media));
        let (pinned, _) = get(&fx.state, &route, TOKEN).await;
        assert_eq!(pinned["TotalRecordCount"], 1);
        assert_eq!(items(&pinned)[0]["Id"], work(media));
        assert_eq!(items(&pinned)[0]["UserData"]["Played"], false);
        fx.state(media, (-1, -1), 0, true, 20);
        let (played, _) = get(&fx.state, &route, TOKEN).await;
        assert_eq!(played["TotalRecordCount"], 0);
        assert!(items(&played).is_empty());
    });
}

fn adopt_browse_members(fx: &Fixture, media: MediaId, genres: &[&str], studios: &[&str]) {
    fx.store()
        .adopt_metadata(
            &domain::MetadataAdoption {
                media_id: media,
                scope: domain::EntityScope::Work,
                locale: "zh-CN".into(),
                source: "tmdb".into(),
                provider_entity_id: "fixture".into(),
                provider_work_id: "fixture".into(),
                fetched_at: 1,
                normalized: domain::NormalizedMetadata {
                    fields: domain::MetadataFields {
                        genres: domain::FieldValue::Value(
                            genres
                                .iter()
                                .map(|value| domain::MetadataGenre {
                                    source: "tmdb".into(),
                                    source_id: None,
                                    filter_key: value.to_lowercase(),
                                    name: (*value).into(),
                                })
                                .collect(),
                        ),
                        studios: domain::FieldValue::Value(
                            studios.iter().map(|value| (*value).into()).collect(),
                        ),
                        ..Default::default()
                    },
                    field_origins: Default::default(),
                },
                expected_revision: Some(0),
                preferences: domain::MetadataPreferences {
                    language_priority: vec!["zh-CN".into()],
                    cert_country_priority: vec![],
                },
            },
            1,
        )
        .unwrap();
}

#[test]
fn resume_studio_parent_falls_back_after_empty_genre_with_exact_total() {
    let fx = Fixture::new();
    let (_, root) = fx.library("resume-studio", MediaKind::Movie, true, true);
    let studio = fx.media(MediaKind::Movie, "Studio continuation");
    let genre = fx.media(MediaKind::Movie, "Genre precedence");
    fx.publish(&root, studio, None, "one");
    fx.publish(&root, genre, None, "one");
    adopt_browse_members(&fx, studio, &[], &["ResumeStudio", "Collision"]);
    adopt_browse_members(&fx, genre, &["Collision"], &[]);
    fx.state(studio, (-1, -1), 10_000, false, 10);
    fx.state(genre, (-1, -1), 20_000, false, 20);
    runtime().block_on(async {
        let route = format!("/Users/{}/Items/Resume", fx.user);
        let (page, _) = get(
            &fx.state,
            &format!("{route}?ParentId=ResumeStudio&Limit=1"),
            TOKEN,
        )
        .await;
        assert_eq!(page["TotalRecordCount"], 1);
        assert_eq!(items(&page)[0]["Id"], work(studio));
        let (end, _) = get(
            &fx.state,
            &format!("{route}?ParentId=ResumeStudio&StartIndex=99&Limit=1"),
            TOKEN,
        )
        .await;
        assert_eq!(
            end["TotalRecordCount"], 1,
            "fallback tests total, not page emptiness"
        );
        assert!(items(&end).is_empty());
        let (collision, _) = get(
            &fx.state,
            &format!("{route}?ParentId=Collision&Limit=1"),
            TOKEN,
        )
        .await;
        assert_eq!(collision["TotalRecordCount"], 1);
        assert_eq!(
            items(&collision)[0]["Id"],
            work(genre),
            "genre parent takes precedence when it has results"
        );
    });
}

#[test]
fn resume_season_and_folder_parents_filter_sources_before_deduplication_and_page() {
    let fx = Fixture::new();
    let first = fx.dir.path().join("resume-root-first");
    let second = fx.dir.path().join("resume-root-second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    fx.store()
        .create_library(
            MediaKind::Tv,
            "Two source roots",
            &[first.to_str().unwrap(), second.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = fx.media(MediaKind::Tv, "Scoped continuation");
    let a = fx.publish(&first, media, Some((1, 1)), "first");
    let b = fx.publish(&second, media, Some((2, 1)), "second");
    fx.state(media, (1, 1), 10_000, false, 10);
    fx.state(media, (2, 1), 20_000, false, 20);
    let season = TypedItemId::Season { media, season: 1 }.to_wire_string();
    let cases = [
        (season, unit(media, 1, 1), a.path.clone()),
        (
            media_server::provider::query::folder_id_for_root(&first),
            unit(media, 1, 1),
            a.path,
        ),
        (
            media_server::provider::query::folder_id_for_root(&second),
            unit(media, 2, 1),
            b.path,
        ),
    ];
    runtime().block_on(async {
        let route = format!("/Users/{}/Items/Resume", fx.user);
        for (parent, wanted, path) in cases {
            let (page, _) = get(
                &fx.state,
                &format!("{route}?ParentId={parent}&Limit=1"),
                TOKEN,
            )
            .await;
            assert_eq!(page["TotalRecordCount"], 1, "parent={parent}");
            assert_eq!(items(&page)[0]["Id"], wanted, "parent={parent}");
            assert_eq!(
                items(&page)[0]["Path"],
                path.display().to_string(),
                "parent={parent}"
            );
            let (end, _) = get(
                &fx.state,
                &format!("{route}?ParentId={parent}&StartIndex=99&Limit=1"),
                TOKEN,
            )
            .await;
            assert_eq!(end["TotalRecordCount"], 1, "parent={parent}");
            assert!(items(&end).is_empty());
        }
    });
}
