#[path = "management/common.rs"]
mod common;

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use axum::http::StatusCode;
use domain::LibraryId;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

#[tokio::test]
async fn api_query_scale_page_and_cursor() {
    let temp = tempfile::tempdir().unwrap();
    let state = common::state(
        temp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
    );
    let app = api::router(state.clone());
    let token = "management-secret";

    let library_id = {
        let binding = state.store();
        let store = binding.as_ref();
        let lib = store
            .list_libraries()
            .unwrap()
            .into_iter()
            .find(|l| l.kind == domain::MediaKind::Movie)
            .unwrap();
        let lib_id: LibraryId = lib.id.parse().unwrap();
        lib_id
    };

    // Bulk insert 100 items into library.db directly
    {
        let db = rusqlite::Connection::open(temp.path().join("data/library.db")).unwrap();
        let tx = db.unchecked_transaction().unwrap();
        let roots = state.store().list_library_roots().unwrap();
        let root = roots
            .into_iter()
            .find(|r| r.library_id.as_str() == library_id.to_string().as_str())
            .unwrap();

        for i in 0..100 {
            let m_id = format!("00000000-0000-0000-0000-{:012x}", i);
            let f_id = format!("00000000-0000-0000-0001-{:012x}", i);
            tx.execute(
                "INSERT INTO media (media_id, kind, title, year, identity_state, aliases_display_json, metadata_revision)
                 VALUES (?1, 'movie', ?2, 2020, 'confirmed', '[]', 0)",
                rusqlite::params![m_id, format!("Movie {:04}", i)],
            ).unwrap();
            tx.execute(
                "INSERT INTO library_files
                    (file_id, library_id, root_id, media_id, kind, path_key, display_path, presence_state,
                     publication_state, file_revision, source_version, stat_known, quality_source, confidence)
                 VALUES (?1, ?2, ?3, ?4, 'movie', ?5, ?5, 'present', 'published', 1, 'v1', 1, 'probe', 'high')",
                rusqlite::params![f_id, library_id.to_string(), root.id, m_id, format!("{}/m{:04}.mkv", root.path.display(), i)],
            ).unwrap();
            tx.execute(
                "INSERT INTO library_items
                    (library_id, media_id, kind, title, title_sort, year, identity_state,
                     metadata_revision, item_revision, release_date_sort, rating_sort, runtime_sort,
                     added_at_sort, language, size_bytes, size_sort, resolution_rank, hdr,
                     scraped, missing, stable_shuffle_key, file_count, available_file_count,
                     missing_file_count, unknown_file_count, distinct_episode_count, distinct_season_count, preferred_file_id)
                 VALUES (?1, ?2, 'movie', ?3, ?3, 2020, 'confirmed',
                         0, 1, 0, 0, 0,
                         ?4, 'en', 1000, 1000, 3, 0,
                         1, 0, ?5, 1, 1, 0, 0, 0, 0, ?6)",
                rusqlite::params![library_id.to_string(), m_id, format!("Movie {:04}", i), i as i64, format!("{:020}", i), f_id],
            ).unwrap();
        }
        tx.commit().unwrap();
    }

    // 1. Regular query with limit=50
    let req = common::request(
        "GET",
        &format!("/api/v1/libraries/{library_id}/items?limit=50"),
        Some(token),
        Value::Null,
    );
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = common::json_data(res).await;
    assert_eq!(body.as_array().unwrap().len(), 50);
}

// ---------------------------------------------------------------------------
// Task 18 Step 1 (plan:404-410) —— HTTP 面可达的观测项。
//
// ③ 排序：空值/NULL、tie、ASC/DESC、Unicode 必须与窗口切分无关。
// ④ 未看 / last_played 过滤，以及同一 Media 跨库时每库各一张卡片。
// ① FullscanStep/VmStep 与 count 快照在本 HTTP 面不可达：响应体里没有
//    QueryWorkStats。这两项由 store 侧 `storage_work_bounds/seek_scale.rs` 覆盖。
// ② 深游标现在**可以**从 HTTP 面走到了（`/items` 用 `cursor` 而不是 offset，
//    见 `library_cursor_paging.rs`），这里的窗口切分用例因此按游标翻页。
// ---------------------------------------------------------------------------

/// 固定夹具。每个字段都是数据库里真实的排序键，期望顺序在 Rust 侧用同一组数字算出，
/// 不把被测顺序再读回来当期望。
struct Card {
    key: usize,
    title: String,
    title_sort: String,
    release_sort: String,
    rating: f64,
    added_at: i64,
}

impl Card {
    fn media_id(&self) -> String {
        format!("00000000-0000-0000-0000-{:012x}", self.key)
    }

    fn file_id(&self, salt: u32) -> String {
        format!("00000000-0000-{salt:04x}-0001-{:012x}", self.key)
    }
}

const CARD_COUNT: usize = 40;
const WATCH_COUNT: usize = 60;
const SHARED_COUNT: usize = 30;
const SECOND_ONLY_COUNT: usize = 10;

/// 原始 SQL 夹具插入需要的库 / 根坐标。
struct LibCoords {
    library_id: String,
    root_id: String,
    root_path: String,
}

fn lib_coords_for(state: &api::ApiState, library_id: &str) -> LibCoords {
    let binding = state.store();
    let store = binding.as_ref();
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id.as_str() == library_id)
        .unwrap();
    LibCoords {
        root_path: root.path.display().to_string(),
        root_id: root.id,
        library_id: library_id.to_string(),
    }
}

fn movie_library_id(state: &api::ApiState) -> String {
    state
        .store()
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|library| library.kind == domain::MediaKind::Movie)
        .unwrap()
        .id
}

fn api_app(temp: &tempfile::TempDir) -> (api::ApiState, axum::Router) {
    let state = common::state(
        temp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
    );
    let app = api::router(state.clone());
    (state, app)
}

fn fixture_cards(total: usize) -> Vec<Card> {
    let unicode = ["Zeta", "Émile", "中文标题", "Ångström", "alpha"];
    (0..total)
        .map(|key| Card {
            key,
            title: format!("{} #{key:02}", unicode[key % unicode.len()]),
            // 相差 20 的 key 同时重复桶位和 Unicode 词 ⇒ 必然产生 tie。
            title_sort: format!("{}{}", key % 4, unicode[key % unicode.len()]),
            // 空字符串是「没有发行日期」的默认桶，ASC 时排在最前。
            release_sort: if key % 7 == 0 {
                String::new()
            } else {
                format!(
                    "{:04}-{:02}-{:02}",
                    1990 + key % 30,
                    1 + key % 12,
                    1 + key % 28
                )
            },
            rating: (key % 5) as f64 * 0.5,
            added_at: 1000 + key as i64,
        })
        .collect()
}

fn seed_cards(db_path: &std::path::Path, coords: &LibCoords, salt: u32, cards: &[Card]) {
    let db = rusqlite::Connection::open(db_path).unwrap();
    let tx = db.unchecked_transaction().unwrap();
    for card in cards {
        tx.execute(
            "INSERT OR IGNORE INTO media (media_id, kind, title, year, identity_state, \
                 aliases_display_json, metadata_revision) \
             VALUES (?1, 'movie', ?2, 2020, 'confirmed', '[]', 0)",
            rusqlite::params![card.media_id(), card.title],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO library_files (file_id, library_id, root_id, media_id, kind, path_key, \
                 display_path, presence_state, publication_state, file_revision, source_version, \
                 stat_known, quality_source, confidence) \
             VALUES (?1, ?2, ?3, ?4, 'movie', ?5, ?5, 'present', 'published', 1, 'v1', 1, \
                 'probe', 'high')",
            rusqlite::params![
                card.file_id(salt),
                coords.library_id,
                coords.root_id,
                card.media_id(),
                format!("{}/m{:04}.mkv", coords.root_path, card.key)
            ],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO library_items (library_id, media_id, kind, title, title_sort, year, \
                 identity_state, metadata_revision, item_revision, release_date_sort, \
                 rating_sort, runtime_sort, added_at_sort, language, size_bytes, \
                 size_sort, resolution_rank, hdr, scraped, missing, stable_shuffle_key, \
                 file_count, available_file_count, missing_file_count, unknown_file_count, \
                 distinct_episode_count, distinct_season_count, preferred_file_id) \
             VALUES (?1, ?2, 'movie', ?3, ?4, 2020, 'confirmed', 0, 1, ?5, ?6, 0, ?7, \
                 'en', 1000, 1000, 3, 0, 1, 0, ?8, 1, 1, 0, 0, 0, 0, ?9)",
            rusqlite::params![
                coords.library_id,
                card.media_id(),
                card.title,
                card.title_sort,
                card.release_sort,
                card.rating,
                card.added_at,
                format!("{:020}", card.key),
                card.file_id(salt)
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

/// `key % 10 == 9` 完全没有 user 行 ⇒ COALESCE 语义下的未看 / last_played = 0 桶。
fn seed_watch_rows(db_path: &std::path::Path, coords: &LibCoords, user_id: &str, total: usize) {
    let db = rusqlite::Connection::open(db_path).unwrap();
    let tx = db.unchecked_transaction().unwrap();
    for key in 0..total {
        if key % 10 == 9 {
            continue;
        }
        tx.execute(
            "INSERT INTO user_library_items (user_id, library_id, media_id, seen, played, \
                 favorite, last_played_at, favorite_at, owned_episode_count, \
                 completed_episode_count, summary_revision) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, 0, 0, 1)",
            rusqlite::params![
                user_id,
                coords.library_id,
                format!("00000000-0000-0000-0000-{key:012x}"),
                i64::from(key % 2 == 0),
                i64::from(key % 6 == 0),
                i64::from(key % 5 == 0),
                1000 + key as i64
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

async fn api_items(app: &axum::Router, token: &str, uri: &str) -> Vec<Value> {
    let response = app
        .clone()
        .oneshot(common::request("GET", uri, Some(token), Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "GET {uri}");
    common::json_data(response)
        .await
        .as_array()
        .unwrap()
        .clone()
}

/// 本页条目 + 下一页游标（`data` 是数组，游标挂在信封上）。
async fn api_page(app: &axum::Router, token: &str, uri: &str) -> (Vec<Value>, Option<String>) {
    let response = app
        .clone()
        .oneshot(common::request("GET", uri, Some(token), Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "GET {uri}");
    let body = common::json_body(response).await;
    let items = body["data"].as_array().cloned().unwrap_or_default();
    (items, body["next_cursor"].as_str().map(str::to_string))
}

/// 游标是 JSON，必须整体转义后再拼进查询串。
fn urlencode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn item_ids(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .map(|item| item["media_item_id"].as_str().unwrap().to_string())
        .collect()
}

fn card_in(items: &[Value], media_id: &str) -> Value {
    items
        .iter()
        .find(|item| item["media_item_id"] == media_id)
        .cloned()
        .unwrap_or_else(|| panic!("missing card {media_id}"))
}

/// 登录真实用户：watch 过滤按 user 维度生效，必须拿到 user_id 才能种 user 行。
async fn admin_session(app: &axum::Router, state: &api::ApiState) -> (String, String) {
    let response = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/auth/login",
            None,
            serde_json::json!({ "username": "admin", "password": "management-secret" }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "admin login");
    let token = common::json_body(response).await["data"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    let user_id = state
        .store()
        .user_id_by_token(&token)
        .unwrap()
        .unwrap()
        .to_string();
    (token, user_id)
}

/// 与 `order_spec` 同构：主键按 sort 选择，tie-break 是 `media_id` 且方向与主键一致。
fn expected_order(cards: &[Card], sort: &str, descending: bool) -> Vec<String> {
    let mut ordered: Vec<&Card> = cards.iter().collect();
    ordered.sort_by(|left, right| {
        let primary = match sort {
            "title" => left.title_sort.as_bytes().cmp(right.title_sort.as_bytes()),
            "release_date" => left
                .release_sort
                .as_bytes()
                .cmp(right.release_sort.as_bytes()),
            "rating" => left.rating.partial_cmp(&right.rating).unwrap(),
            "added_at" => left.added_at.cmp(&right.added_at),
            // 这里没有 user 行 ⇒ COALESCE(last_played_at, 0) 全是 0，纯 tie-break 顺序。
            "last_played" => std::cmp::Ordering::Equal,
            other => panic!("unhandled sort {other}"),
        };
        primary.then_with(|| left.media_id().cmp(&right.media_id()))
    });
    if descending {
        ordered.reverse();
    }
    ordered.into_iter().map(Card::media_id).collect()
}

#[tokio::test]
async fn api_ordering_windows_are_stable_across_blank_ties_and_unicode() {
    let temp = tempfile::tempdir().unwrap();
    let (state, app) = api_app(&temp);
    let token = "management-secret";
    let coords = lib_coords_for(&state, &movie_library_id(&state));
    let cards = fixture_cards(CARD_COUNT);
    seed_cards(&temp.path().join("data/library.db"), &coords, 1, &cards);

    for (sort, order) in [
        ("title", "asc"),
        ("title", "desc"),
        ("release_date", "asc"),
        ("release_date", "desc"),
        ("rating", "asc"),
        ("rating", "desc"),
        ("added_at", "desc"),
        ("last_played", "asc"),
    ] {
        let expected = expected_order(&cards, sort, order == "desc");
        let base = format!(
            "/api/v1/libraries/{}/items?sort={sort}&order={order}",
            coords.library_id
        );
        let one_shot = item_ids(&api_items(&app, token, &format!("{base}&limit=200")).await);
        assert_eq!(one_shot, expected, "one-shot sort={sort} order={order}");

        // 分页窗口：键集游标逐页翻完（offset 已删除）。
        let mut windowed = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let uri = match &cursor {
                None => format!("{base}&limit=7"),
                Some(raw) => format!("{base}&limit=7&cursor={}", urlencode(raw)),
            };
            let (page, next) = api_page(&app, token, &uri).await;
            windowed.extend(item_ids(&page));
            match next {
                Some(raw) => cursor = Some(raw),
                None => break,
            }
            assert!(
                windowed.len() <= CARD_COUNT,
                "游标没有收敛 sort={sort} order={order}"
            );
        }
        assert_eq!(
            windowed, expected,
            "cursor windows sort={sort} order={order}"
        );
        assert_eq!(
            windowed.iter().collect::<BTreeSet<_>>().len(),
            CARD_COUNT,
            "duplicate ids sort={sort} order={order}"
        );
    }

    // 空 release_date 是默认桶：ASC 排在最前，桶内按 media_id 升序。
    let mut blank: Vec<String> = cards
        .iter()
        .filter(|card| card.release_sort.is_empty())
        .map(Card::media_id)
        .collect();
    blank.sort();
    let ascending = item_ids(
        &api_items(
            &app,
            token,
            &format!(
                "/api/v1/libraries/{}/items?sort=release_date&order=asc&limit=200",
                coords.library_id
            ),
        )
        .await,
    );
    assert_eq!(
        &ascending[..blank.len()],
        &blank[..],
        "blank release_date first"
    );
}

struct WatchMembers {
    unwatched: BTreeSet<String>,
    seen: BTreeSet<String>,
    played: BTreeSet<String>,
    watching: BTreeSet<String>,
    favorite: BTreeSet<String>,
}

fn watch_members(total: usize) -> WatchMembers {
    let mut members = WatchMembers {
        unwatched: BTreeSet::new(),
        seen: BTreeSet::new(),
        played: BTreeSet::new(),
        watching: BTreeSet::new(),
        favorite: BTreeSet::new(),
    };
    for key in 0..total {
        let id = format!("00000000-0000-0000-0000-{key:012x}");
        let seen = key % 2 == 0;
        let played = key % 6 == 0;
        if !seen {
            members.unwatched.insert(id.clone());
        }
        if seen {
            members.seen.insert(id.clone());
        }
        if played {
            members.played.insert(id.clone());
        }
        if seen && !played {
            members.watching.insert(id.clone());
        }
        if key % 5 == 0 {
            members.favorite.insert(id.clone());
        }
    }
    assert_eq!(
        (
            members.unwatched.len(),
            members.seen.len(),
            members.played.len(),
            members.watching.len(),
            members.favorite.len()
        ),
        (30, 30, 10, 20, 12),
        "watch membership formula drifted"
    );
    members
}

/// `unwatched_first` 的 tier：未看 0 → 已看未播 1 → 已播 2，tier 内按 added_at、media_id 升序。
fn tiered_order(cards: &[Card]) -> Vec<String> {
    let tier = |key: usize| {
        if key % 2 == 1 {
            0
        } else if key.is_multiple_of(6) {
            2
        } else {
            1
        }
    };
    let mut ordered: Vec<&Card> = cards.iter().collect();
    ordered.sort_by_key(|card| (tier(card.key), card.added_at, card.key));
    ordered.into_iter().map(Card::media_id).collect()
}

/// `sort=last_played&order=desc`：有 user 行的按 last_played_at 降序，没有 user 行的 0 桶垫底。
fn last_played_order(cards: &[Card]) -> Vec<String> {
    let played_at = |card: &Card| {
        if card.key % 10 == 9 {
            0
        } else {
            1000 + card.key as i64
        }
    };
    let mut ordered: Vec<&Card> = cards.iter().collect();
    ordered.sort_by(|left, right| {
        played_at(right)
            .cmp(&played_at(left))
            .then_with(|| right.media_id().cmp(&left.media_id()))
    });
    ordered.into_iter().map(Card::media_id).collect()
}

#[tokio::test]
async fn api_watch_filters_and_unwatched_first_over_http() {
    let temp = tempfile::tempdir().unwrap();
    let (state, app) = api_app(&temp);
    let coords = lib_coords_for(&state, &movie_library_id(&state));
    let cards = fixture_cards(WATCH_COUNT);
    let db_path = temp.path().join("data/library.db");
    seed_cards(&db_path, &coords, 1, &cards);
    let (token, user_id) = admin_session(&app, &state).await;
    seed_watch_rows(&db_path, &coords, &user_id, WATCH_COUNT);

    let members = watch_members(WATCH_COUNT);
    let base = format!("/api/v1/libraries/{}/items", coords.library_id);
    for (watch, expected) in [
        ("unwatched", &members.unwatched),
        ("seen", &members.seen),
        ("played", &members.played),
        ("watching", &members.watching),
        ("favorite", &members.favorite),
    ] {
        let ids = item_ids(
            &api_items(
                &app,
                &token,
                &format!("{base}?w={watch}&sort=added_at&order=asc&limit=200"),
            )
            .await,
        );
        let got: BTreeSet<String> = ids.iter().cloned().collect();
        assert_eq!(&got, expected, "watch={watch}");
    }

    let tiered = item_ids(
        &api_items(
            &app,
            &token,
            &format!("{base}?unwatched_first=true&sort=added_at&order=asc&limit=200"),
        )
        .await,
    );
    assert_eq!(tiered, tiered_order(&cards), "unwatched_first tiers");

    let by_last_played = item_ids(
        &api_items(
            &app,
            &token,
            &format!("{base}?sort=last_played&order=desc&limit=200"),
        )
        .await,
    );
    assert_eq!(
        by_last_played,
        last_played_order(&cards),
        "last_played desc"
    );
}

#[tokio::test]
async fn api_cross_library_media_keeps_one_card_per_library() {
    let temp = tempfile::tempdir().unwrap();
    let (state, app) = api_app(&temp);
    let token = "management-secret";
    let first = lib_coords_for(&state, &movie_library_id(&state));
    let created = state
        .store()
        .create_library(
            domain::MediaKind::Movie,
            "Second",
            &["/second"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let second = lib_coords_for(&state, &created.id);

    let cards = fixture_cards(SHARED_COUNT + SECOND_ONLY_COUNT);
    let (shared, second_only) = cards.split_at(SHARED_COUNT);
    assert_eq!(second_only.len(), SECOND_ONLY_COUNT);
    let db_path = temp.path().join("data/library.db");
    seed_cards(&db_path, &first, 1, shared);
    seed_cards(&db_path, &second, 2, &cards);

    let shared_ids: BTreeSet<String> = shared.iter().map(Card::media_id).collect();
    let all_ids: BTreeSet<String> = cards.iter().map(Card::media_id).collect();
    let first_items = api_items(
        &app,
        token,
        &format!(
            "/api/v1/libraries/{}/items?sort=title&order=asc&limit=200",
            first.library_id
        ),
    )
    .await;
    let second_items = api_items(
        &app,
        token,
        &format!(
            "/api/v1/libraries/{}/items?sort=title&order=asc&limit=200",
            second.library_id
        ),
    )
    .await;
    assert_eq!(
        item_ids(&first_items).into_iter().collect::<BTreeSet<_>>(),
        shared_ids,
        "first library cards"
    );
    assert_eq!(
        item_ids(&second_items).into_iter().collect::<BTreeSet<_>>(),
        all_ids,
        "second library cards"
    );

    // 同一个 Media 在两个库各有一条文件：每个库仍只看到自己那份，
    // 卡片上的 preferred file 按本库解析（poster_url 里带的就是本库的 file_id）。
    for (coords, items, salt) in [(&first, &first_items, 1u32), (&second, &second_items, 2u32)] {
        for card in shared {
            let json = card_in(items, &card.media_id());
            assert_eq!(json["library_id"].as_str().unwrap(), coords.library_id);
            assert_eq!(
                json["file_count"].as_u64().unwrap(),
                1,
                "per-library file_count"
            );
            assert!(
                json["poster_url"]
                    .as_str()
                    .unwrap()
                    .ends_with(&card.file_id(salt)),
                "per-library preferred file for {}",
                card.media_id()
            );
        }
    }
}
