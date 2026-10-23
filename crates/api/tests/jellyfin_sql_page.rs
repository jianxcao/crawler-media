//! Task 11 步骤 1 回归：`Store::protocol_item_page` 的**全部合同字段**必须在 SQL 内
//! 选完再分页，页内 `TypedItemId`/授权 `preferred_source`/`filtered total` 一次性给出。
//!
//! 断言只用公开缝：`Store::protocol_item_page`（api re-export）+ `domain` 合同类型。
//! fixture 通过 `publish_fixture_file` 走真实发布路径，所以 NFO 只在此刻被观察一次，
//! 之后页查询不得再读现场 NFO。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use api::Store;
use domain::{
    Confidence, EntityScope, FieldValue, LedgerId, LibraryId, Media, MediaId, MediaKind,
    MetadataAdoption, MetadataFields, MetadataPreferences, NormalizedMetadata, ProtocolItemQuery,
    ProtocolItemType, ProtocolMarkFilter, ProtocolParent, ProtocolSort, ProtocolSortKey,
    QualitySource, TypedItemId, UnitKey, UserId,
};

const ALICE_NFO: &str = "<movie><plot>A needle in the overview.</plot>\
<genre>Drama</genre><studio>Studio X</studio>\
<actor><name>Alice</name><role>Actor</role></actor></movie>";

struct Fx {
    _dir: tempfile::TempDir,
    store: Store,
    alpha: LibraryId,
    alpha_root: PathBuf,
    beta: LibraryId,
    beta_root: PathBuf,
}

impl Fx {
    /// 两个 Movie 库：`alpha` 用于绝大多数场景，`beta` 用于跨库/隐藏库场景。
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let alpha_root = dir.path().join("alpha");
        let beta_root = dir.path().join("beta");
        std::fs::create_dir_all(&alpha_root).unwrap();
        std::fs::create_dir_all(&beta_root).unwrap();
        let alpha = create_library(&store, "Alpha", &alpha_root);
        let beta = create_library(&store, "Beta", &beta_root);
        Self {
            _dir: dir,
            store,
            alpha,
            alpha_root,
            beta,
            beta_root,
        }
    }

    fn add_in(&self, root: &Path, title: &str, nfo: Option<&str>) -> MediaId {
        let media = insert_media(&self.store, title, MediaKind::Movie);
        let path = root.join(format!("{title}.mkv"));
        std::fs::write(&path, b"payload").unwrap();
        if let Some(body) = nfo {
            std::fs::write(root.join(format!("{title}.nfo")), body).unwrap();
        }
        publish_fixture(&self.store, media, &path, None, None);
        media
    }

    fn add_movie(&self, title: &str) -> MediaId {
        self.add_in(&self.alpha_root, title, None)
    }

    fn add_movie_with_nfo(&self, title: &str, nfo: &str) -> MediaId {
        self.add_in(&self.alpha_root, title, Some(nfo))
    }

    /// 同一逻辑作品的另一个版本：换一个库根写文件，但沿用同一个 `MediaId`。
    fn add_version(&self, root: &Path, media: MediaId, title: &str) {
        let path = root.join(format!("{title}.mkv"));
        std::fs::write(&path, b"payload").unwrap();
        publish_fixture(&self.store, media, &path, None, None);
    }

    /// TV 库里一部剧的分集；返回 series media。
    fn add_show(&self, dir: &Path, episodes: &[(u32, u32)]) -> MediaId {
        let media = insert_media(&self.store, "Show", MediaKind::Tv);
        let show_dir = dir.join("Show");
        std::fs::create_dir_all(&show_dir).unwrap();
        for (season, episode) in episodes {
            let path = show_dir.join(format!("S{season:02}E{episode:02}.mkv"));
            std::fs::write(&path, b"payload").unwrap();
            publish_fixture(&self.store, media, &path, Some(*season), Some(*episode));
        }
        media
    }
}

fn create_library(store: &Store, name: &str, root: &Path) -> LibraryId {
    store
        .create_library(
            MediaKind::Movie,
            name,
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap()
        .id
        .parse()
        .unwrap()
}

fn insert_media(store: &Store, title: &str, kind: MediaKind) -> MediaId {
    let media = Media {
        id: MediaId::new(),
        kind,
        title: title.into(),
        year: Some(2024),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: Vec::new(),
    };
    store.insert_media(&media).unwrap();
    media.id
}

fn publish_fixture(
    store: &Store,
    media: MediaId,
    path: &Path,
    season: Option<u32>,
    episode: Option<u32>,
) -> LedgerId {
    let spec = store::test_support::FixtureFile {
        media_id: media,
        path: path.display().to_string(),
        season,
        episode,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        ..Default::default()
    };
    store::test_support::publish_fixture_file(store, &spec)
        .unwrap()
        .file_id
}

fn base_query(parent: ProtocolParent) -> ProtocolItemQuery {
    ProtocolItemQuery {
        user_id: UserId::new(),
        parent,
        series_id: None,
        season: None,
        person_ids: Vec::new(),
        studio_ids: Vec::new(),
        search_term: None,
        genres: Vec::new(),
        genre_ids: Vec::new(),
        filters: Vec::new(),
        is_favorite: None,
        is_played: None,
        include_types: vec![ProtocolItemType::Movie],
        recursive: true,
        is_folder: None,
        sort: vec![ProtocolSort {
            key: ProtocolSortKey::Name,
            descending: false,
        }],
        offset: 0,
        limit: 10,
    }
}

fn person_wire_id(key: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, key.as_bytes()).to_string()
}

/// 通过公开采纳缝写入一条 work 范围的 studio 事实（tmdb 来源，`zh-CN`）。
fn adopt_studio(fx: &Fx, media: MediaId, studio: &str) {
    let fields = MetadataFields {
        studios: FieldValue::Value(vec![studio.to_string()]),
        ..Default::default()
    };
    let adoption = MetadataAdoption {
        media_id: media,
        scope: EntityScope::Work,
        locale: "zh-CN".into(),
        source: "tmdb".into(),
        provider_entity_id: "100".into(),
        provider_work_id: "100".into(),
        fetched_at: 1,
        normalized: NormalizedMetadata {
            fields,
            field_origins: BTreeMap::new(),
        },
        expected_revision: Some(0),
        preferences: MetadataPreferences {
            language_priority: vec!["zh-CN".into()],
            cert_country_priority: Vec::new(),
        },
    };
    fx.store.adopt_metadata(&adoption, 1).unwrap();
}

/// 人物命中排在第 51 位：必须在 LIMIT 之前被过滤，total 也是过滤后的值。
#[test]
fn person_match_beyond_first_page_is_filtered_before_limit() {
    let fx = Fx::new();
    let mut target = None;
    for index in 0..51 {
        let title = format!("Item{index:03}");
        let media = if index == 50 {
            fx.add_movie_with_nfo(&title, ALICE_NFO)
        } else {
            fx.add_movie(&title)
        };
        target = Some(media);
    }
    let expected = target.unwrap();

    let mut query = base_query(ProtocolParent::Library(fx.alpha));
    query.person_ids = vec![person_wire_id("name:alice:Actor")];
    query.limit = 1;

    let page = fx.store.protocol_item_page(&query).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].item, TypedItemId::Work(expected));
}

/// 仅简介命中 + 仅 studio 命中 + genre 命中：同样先过滤后分页。
///
/// studio 走真实采纳缝（`Store::adopt_metadata`）：`crates/library/src/nfo/fields.rs`
/// 当前不把 `<studio>` 映射进 `MetadataFields.studios`，所以 NFO 侧carry 不出 studio 事实，
/// 本地 studio 只有采纳元数据这一条来源，fixture 也从这条来源造。
#[test]
fn overview_studio_and_genre_matches_are_filtered_before_limit() {
    let fx = Fx::new();
    let mut target = None;
    for index in 0..51 {
        let title = format!("Item{index:03}");
        let media = if index == 50 {
            fx.add_movie_with_nfo(&title, ALICE_NFO)
        } else {
            fx.add_movie(&title)
        };
        target = Some(media);
    }
    let expected = target.unwrap();
    adopt_studio(&fx, expected, "Studio X");

    for mutate in [
        |query: &mut ProtocolItemQuery| query.search_term = Some("needle".into()),
        |query: &mut ProtocolItemQuery| query.studio_ids = vec!["Studio X".into()],
        |query: &mut ProtocolItemQuery| query.genres = vec!["Drama".into()],
    ] {
        let mut query = base_query(ProtocolParent::Library(fx.alpha));
        query.limit = 1;
        mutate(&mut query);
        let page = fx.store.protocol_item_page(&query).unwrap();
        assert_eq!(page.total, 1, "filtered total for {query:?}");
        assert_eq!(page.items.len(), 1, "page size for {query:?}");
        assert_eq!(page.items[0].item, TypedItemId::Work(expected));
    }
}

/// 同一逻辑作品跨库多版本（不同 file）在 Root 范围只出现一次，并给出授权 preferred_source。
#[test]
fn cross_library_multiversion_same_logical_id_is_unique() {
    let fx = Fx::new();
    let shared = fx.add_movie("Shared");
    fx.add_version(&fx.beta_root, shared, "Shared");

    let query = base_query(ProtocolParent::Root);
    let page = fx.store.protocol_item_page(&query).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].item, TypedItemId::Work(shared));
    let source = page.items[0]
        .preferred_source
        .as_ref()
        .expect("authorized preferred source");
    assert!(source.to_wire_string().starts_with("f1_"));
}

/// 已进入移除流程的库不得泄露；未知 parent 返回空页。
#[test]
fn removed_library_and_unknown_parent_do_not_leak() {
    let fx = Fx::new();
    let alpha_item = fx.add_movie("Alpha Movie");
    fx.add_in(&fx.beta_root, "Beta Movie", None);
    fx.store
        .begin_library_removal(&fx.beta.to_string())
        .unwrap();

    let root_page = fx
        .store
        .protocol_item_page(&base_query(ProtocolParent::Root))
        .unwrap();
    assert_eq!(root_page.total, 1);
    assert_eq!(root_page.items[0].item, TypedItemId::Work(alpha_item));

    let beta_page = fx
        .store
        .protocol_item_page(&base_query(ProtocolParent::Library(fx.beta)))
        .unwrap();
    assert_eq!(beta_page.total, 0);
    assert!(beta_page.items.is_empty());

    let unknown_page = fx
        .store
        .protocol_item_page(&base_query(ProtocolParent::Unknown))
        .unwrap();
    assert_eq!(unknown_page.total, 0);
    assert!(unknown_page.items.is_empty());
}

/// 多键排序：Name 之后仍按第二键（DateCreated）决定顺序，且大小写/Unicode 稳定。
#[test]
fn multi_key_sort_is_stable_for_unicode_and_case_ties() {
    let fx = Fx::new();
    let upper = fx.add_movie("A");
    let lower = fx.add_movie("a");
    let last = fx.add_movie("B");
    let unicode = fx.add_movie("中");

    let mut query = base_query(ProtocolParent::Library(fx.alpha));
    query.sort = vec![
        ProtocolSort {
            key: ProtocolSortKey::Name,
            descending: false,
        },
        ProtocolSort {
            key: ProtocolSortKey::DateCreated,
            descending: false,
        },
    ];
    let page = fx.store.protocol_item_page(&query).unwrap();
    assert_eq!(page.total, 4);
    let order = page
        .items
        .iter()
        .map(|item| item.item.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![
            TypedItemId::Work(upper),
            TypedItemId::Work(lower),
            TypedItemId::Work(last),
            TypedItemId::Work(unicode),
        ]
    );

    query.sort[0].descending = true;
    let page = fx.store.protocol_item_page(&query).unwrap();
    assert_eq!(
        page.items.first().map(|item| item.item.clone()),
        Some(TypedItemId::Work(unicode))
    );
}

/// Series/Season/Folder parent 只展开局部单元，并保留 filtered total。
#[test]
fn series_season_and_folder_parents_stay_local() {
    let fx = Fx::new();
    let dir = fx._dir.path().join("tv");
    std::fs::create_dir_all(&dir).unwrap();
    create_library_tv(&fx.store, "Shows", &dir);
    let show = fx.add_show(&dir, &[(1, 1), (1, 2), (2, 1)]);

    let mut season_query = base_query(ProtocolParent::Season(show, 1));
    season_query.series_id = Some(show);
    season_query.season = Some((show, 1));
    season_query.include_types = vec![ProtocolItemType::Episode];
    let season_page = fx.store.protocol_item_page(&season_query).unwrap();
    assert_eq!(season_page.total, 2);
    assert_eq!(season_page.items.len(), 2);

    let mut series_query = base_query(ProtocolParent::Series(show));
    series_query.include_types = vec![ProtocolItemType::Episode];
    let series_page = fx.store.protocol_item_page(&series_query).unwrap();
    assert_eq!(series_page.total, 3);

    let mut folder_query = base_query(ProtocolParent::Root);
    folder_query.include_types = vec![ProtocolItemType::Episode];
    let folder_page = fx.store.protocol_item_page(&folder_query).unwrap();
    assert_eq!(folder_page.total, 3);
}

fn create_library_tv(store: &Store, name: &str, root: &Path) -> LibraryId {
    store
        .create_library(
            MediaKind::Tv,
            name,
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap()
        .id
        .parse()
        .unwrap()
}

/// played/favorite 过滤在 SQL 内生效（`filters` 与 `is_played`/`is_favorite` 一致）。
#[test]
fn mark_filters_are_applied_before_paging() {
    let fx = Fx::new();
    let first = fx.add_movie("Aaa");
    fx.add_movie("Bbb");
    let user = UserId::new();
    fx.store
        .mutate_playback(
            first,
            &store::playback::PlaybackMutation {
                user_id: user,
                scope: domain::UnitScope::Whole,
                played: Some(true),
                favorite: None,
                expected_generation: None,
            },
            1,
        )
        .unwrap();

    let mut query = base_query(ProtocolParent::Library(fx.alpha));
    query.user_id = user;
    query.is_played = Some(true);
    query.limit = 1;
    let page = fx.store.protocol_item_page(&query).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].item, TypedItemId::Work(first));

    query.is_played = None;
    query.filters = vec![ProtocolMarkFilter::Unplayed];
    let page = fx.store.protocol_item_page(&query).unwrap();
    assert_eq!(page.total, 1);
    assert_ne!(page.items[0].item, TypedItemId::Work(first));
}

/// `IsFolder=false` 只排除 `is_series=true` 的作品；分集层（unit）不得被它清空。
///
/// 旧实现把 `is_folder == Some(false)` 翻成 `kind <> 'tv'`，再叠上分集层的
/// `kind = 'tv'`，两个条件互斥，整页恒空。
#[test]
fn is_folder_false_at_unit_level_keeps_episodes() {
    let fx = Fx::new();
    let dir = fx._dir.path().join("tv-is-folder");
    std::fs::create_dir_all(&dir).unwrap();
    let library = create_library_tv(&fx.store, "Shows", &dir);
    let show = fx.add_show(&dir, &[(1, 1), (1, 2)]);

    let mut query = base_query(ProtocolParent::Library(library));
    query.include_types = vec![ProtocolItemType::Episode];
    query.is_folder = Some(false);
    let page = fx.store.protocol_item_page(&query).unwrap();
    assert_eq!(page.total, 2, "分集层不得被 IsFolder=false 清空");
    assert_eq!(page.items.len(), 2);
    let ids: Vec<TypedItemId> = page.items.iter().map(|row| row.item.clone()).collect();
    assert!(ids.contains(&TypedItemId::Unit {
        media: show,
        key: UnitKey::new(1, 1),
    }));
    assert!(ids.contains(&TypedItemId::Unit {
        media: show,
        key: UnitKey::new(1, 2),
    }));
}

/// `IncludeItemTypes=Series,Episode` 是混合粒度：剧（work）与分集（unit）同页，
/// total 是两者之和，且 offset/limit 在合并后切分（不得各切一半）。
#[test]
fn mixed_series_and_episode_types_share_one_page() {
    let fx = Fx::new();
    let dir = fx._dir.path().join("tv-mixed");
    std::fs::create_dir_all(&dir).unwrap();
    let library = create_library_tv(&fx.store, "Mixed", &dir);
    let show = fx.add_show(&dir, &[(1, 1)]);

    let mut query = base_query(ProtocolParent::Library(library));
    query.include_types = vec![ProtocolItemType::Series, ProtocolItemType::Episode];
    let page = fx.store.protocol_item_page(&query).unwrap();
    assert_eq!(page.total, 2, "剧 + 分集同页：{page:?}");
    let ids: Vec<TypedItemId> = page.items.iter().map(|row| row.item.clone()).collect();
    assert_eq!(ids.len(), 2, "{page:?}");
    assert!(ids.contains(&TypedItemId::Work(show)), "{page:?}");
    assert!(
        ids.contains(&TypedItemId::Unit {
            media: show,
            key: UnitKey::new(1, 1),
        }),
        "{page:?}"
    );

    let mut first = query.clone();
    first.limit = 1;
    let first_page = fx.store.protocol_item_page(&first).unwrap();
    assert_eq!(first_page.total, 2);
    assert_eq!(first_page.items.len(), 1);
    let mut second = query.clone();
    second.limit = 1;
    second.offset = 1;
    let second_page = fx.store.protocol_item_page(&second).unwrap();
    assert_eq!(second_page.total, 2);
    assert_eq!(second_page.items.len(), 1);
    assert_ne!(
        first_page.items[0].item, second_page.items[0].item,
        "offset/limit 必须在合并后的两粒度上切分"
    );
}

/// R12: the SQL played filter resolves assertions like `effective_played_assertion`: the
/// newest change wins, a tie goes to the more specific scope.
#[test]
fn played_filter_follows_the_newest_assertion() {
    let fx = Fx::new();
    let dir = fx._dir.path().join("tv-played-time");
    std::fs::create_dir_all(&dir).unwrap();
    let library = create_library_tv(&fx.store, "Played", &dir);
    let show = fx.add_show(&dir, &[(1, 1)]);
    let user = UserId::new();
    let mark = |scope: domain::UnitScope, played: bool, now: i64| {
        fx.store
            .mutate_playback(
                show,
                &store::playback::PlaybackMutation {
                    user_id: user,
                    scope,
                    played: Some(played),
                    favorite: None,
                    expected_generation: None,
                },
                now,
            )
            .unwrap();
    };
    mark(
        domain::UnitScope::Episode {
            season: 1,
            episode: 1,
        },
        true,
        1_000,
    );
    mark(domain::UnitScope::Season(1), false, 2_000);
    let mut query = base_query(ProtocolParent::Library(library));
    query.user_id = user;
    query.include_types = vec![ProtocolItemType::Episode];
    query.is_played = Some(true);
    let page = fx.store.protocol_item_page(&query).unwrap();
    assert_eq!(
        page.total, 0,
        "season false@2000 beats episode true@1000: {page:?}"
    );
    let rust = fx
        .store
        .effective_played_assertion(user, show, UnitKey::new(1, 1))
        .unwrap();
    assert!(rust.is_none_or(|a| !a.played), "Rust and SQL agree");
}

/// R23: search folds Unicode case the same way on both sides (ÉCOLE ↔ école).
#[test]
fn unicode_search_folds_case_like_the_display() {
    let fx = Fx::new();
    let ecole = fx.add_movie("ÉCOLE");
    fx.add_movie("Other");
    let mut query = base_query(ProtocolParent::Library(fx.alpha));
    query.search_term = Some("école".into());
    let unicode_page = fx.store.protocol_item_page(&query).unwrap();
    assert_eq!(unicode_page.total, 1);
    assert_eq!(unicode_page.items[0].item, TypedItemId::Work(ecole));
}

/// R23: titles equal under case folding are ordered by the *next* requested key
/// (DateCreated desc), not by the raw title bytes.
#[test]
fn case_equal_names_fall_through_to_the_next_sort_key() {
    let fx = Fx::new();
    let upper_a_id = fx.add_movie("A");
    let lower_a_id = fx.add_movie("a");
    let db = rusqlite::Connection::open(fx._dir.path().join("data").join("library.db")).unwrap();
    for (media, added) in [(upper_a_id, 100_i64), (lower_a_id, 200)] {
        db.execute(
            "UPDATE library_items SET added_at_sort = ?2 WHERE media_id = ?1",
            rusqlite::params![media.to_string(), added],
        )
        .unwrap();
    }
    let mut query = base_query(ProtocolParent::Library(fx.alpha));
    query.sort = vec![
        ProtocolSort {
            key: ProtocolSortKey::Name,
            descending: false,
        },
        ProtocolSort {
            key: ProtocolSortKey::DateCreated,
            descending: true,
        },
    ];
    let page = fx.store.protocol_item_page(&query).unwrap();
    let case_tie_ids: Vec<_> = page.items.iter().map(|item| item.item.clone()).collect();
    assert_eq!(
        case_tie_ids,
        vec![TypedItemId::Work(lower_a_id), TypedItemId::Work(upper_a_id)]
    );
}

/// R23: a genre id derived from the displayed name ("Drama") filters to that genre.
#[test]
fn genre_id_from_the_displayed_name_round_trips() {
    let fx = Fx::new();
    let drama = fx.add_movie_with_nfo("Dramatic", ALICE_NFO);
    fx.add_movie("Plain");
    let db = rusqlite::Connection::open(fx._dir.path().join("data").join("library.db")).unwrap();
    // Only the NFO carries the genre: no derived term row may paper over the lookup.
    db.execute(
        "DELETE FROM library_item_terms WHERE term_kind = 'genre'",
        [],
    )
    .unwrap();
    let mut query = base_query(ProtocolParent::Library(fx.alpha));
    query.genre_ids = vec![uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, b"Drama").to_string()];
    let genre_roundtrip_page = fx.store.protocol_item_page(&query).unwrap();
    assert_eq!(genre_roundtrip_page.total, 1);
    assert_eq!(genre_roundtrip_page.items[0].item, TypedItemId::Work(drama));
}

fn library_db(fx: &Fx) -> rusqlite::Connection {
    rusqlite::Connection::open(fx._dir.path().join("data").join("library.db")).unwrap()
}

/// R23: an Episode search matches the episode's own title/overview, not the
/// series' work-level fields.
#[test]
fn episode_search_reads_episode_level_fields() {
    let fx = Fx::new();
    let dir = fx._dir.path().join("tv-episode-search");
    std::fs::create_dir_all(&dir).unwrap();
    let library = create_library_tv(&fx.store, "Shows", &dir);
    let show = fx.add_show(&dir, &[(1, 1), (1, 2)]);
    library_db(&fx)
        .execute(
            "INSERT INTO media_episodes (media_id, season, episode, locale, title, overview, revision)
             VALUES (?1, 1, 2, 'zh-CN', 'Second', 'An episode needle.', 1)",
            rusqlite::params![show.to_string()],
        )
        .unwrap();

    let mut query = base_query(ProtocolParent::Library(library));
    query.include_types = vec![ProtocolItemType::Episode];
    query.search_term = Some("NEEDLE".into());
    let page = fx.store.protocol_item_page(&query).unwrap();
    assert_eq!(page.total, 1, "{page:?}");
    assert_eq!(
        page.items[0].item,
        TypedItemId::Unit {
            media: show,
            key: UnitKey::new(1, 2),
        }
    );
    query.search_term = Some("Show".into());
    let page = fx.store.protocol_item_page(&query).unwrap();
    assert_eq!(page.total, 0, "the series title is not an episode field");
}

/// R23: adopted work fields are read in the Library display locale, not the
/// lexically first locale.
#[test]
fn search_reads_the_library_display_locale() {
    let fx = Fx::new();
    let movie = fx.add_movie("Localized");
    let db = library_db(&fx);
    for (locale, overview) in [("en-US", "english plot"), ("zh-CN", "chinese plot")] {
        db.execute(
            "INSERT INTO media_metadata (media_id, locale, title, overview, revision)
             VALUES (?1, ?2, 'Localized', ?3, 1)",
            rusqlite::params![movie.to_string(), locale, overview],
        )
        .unwrap();
    }
    db.execute(
        "UPDATE libraries SET display_locale = 'zh-CN' WHERE library_id = ?1",
        rusqlite::params![fx.alpha.to_string()],
    )
    .unwrap();
    let mut query = base_query(ProtocolParent::Library(fx.alpha));
    query.search_term = Some("chinese".into());
    assert_eq!(fx.store.protocol_item_page(&query).unwrap().total, 1);
    query.search_term = Some("english".into());
    assert_eq!(
        fx.store.protocol_item_page(&query).unwrap().total,
        0,
        "the lexically first locale must not stand in for the display locale"
    );
}

/// R12: a Series row's played filter is reduced per episode: the series is played
/// only when every episode's effective assertion (newest wins, then the more
/// specific scope) is played.
#[test]
fn series_played_filter_reduces_per_episode() {
    let fx = Fx::new();
    let dir = fx._dir.path().join("tv-series-played");
    std::fs::create_dir_all(&dir).unwrap();
    let library = create_library_tv(&fx.store, "Series Played", &dir);
    let show = fx.add_show(&dir, &[(1, 1), (1, 2)]);
    let user = UserId::new();
    let mark = |scope: domain::UnitScope, played: bool, now: i64| {
        fx.store
            .mutate_playback(
                show,
                &store::playback::PlaybackMutation {
                    user_id: user,
                    scope,
                    played: Some(played),
                    favorite: None,
                    expected_generation: None,
                },
                now,
            )
            .unwrap();
    };
    let total = |played: bool| {
        let mut query = base_query(ProtocolParent::Library(library));
        query.user_id = user;
        query.include_types = vec![ProtocolItemType::Series];
        query.is_played = Some(played);
        fx.store.protocol_item_page(&query).unwrap().total
    };
    let episode = |episode: u32| domain::UnitScope::Episode { season: 1, episode };

    mark(episode(1), true, 1_000);
    assert_eq!(
        total(true),
        0,
        "one of two episodes played is not a played series"
    );
    assert_eq!(total(false), 1);

    mark(episode(2), true, 2_000);
    assert_eq!(total(true), 1, "every episode played => played series");
    assert_eq!(total(false), 0);

    mark(domain::UnitScope::Whole, true, 3_000);
    mark(episode(2), false, 4_000);
    assert_eq!(
        total(true),
        0,
        "a newer episode unplayed beats an older whole-series played"
    );
    assert_eq!(total(false), 1);
}
