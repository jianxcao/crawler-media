//! Task 8 Step-1 回归：NFO 输入必须按「实体层 + locale + 库」分开观察与读取。
//!
//! 这里只走公共 seam：`observe_nfo_inputs` 的候选结果、`publish_files` 的落库、以及
//! `library_item_detail` 的卡片读取。层级独立性在卡片上只能验证「不泄漏/不覆盖」——
//! season/episode 层的输入目前没有公开读取器（`library_item_detail` 只读 `entity_scope='work'`）。

use std::path::{Path, PathBuf};

use domain::{
    Confidence, EntityScope, FieldValue, FilePresence, FilePublication, FileVersion, LibraryFile,
    LibraryId, MediaId, MediaKind, QualitySource, UnitKey,
};
use store::Store;
use store::library_write::{FilePublication as StorePublication, WriteScope};

fn library_with_root(store: &Store, dir: &Path, name: &str, kind: MediaKind) -> (String, String) {
    std::fs::create_dir_all(dir).unwrap();
    let library = store
        .create_library(kind, name, &[dir.to_str().unwrap()], "everyone", true, &[])
        .unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id.as_deref() == Some(&library.id))
        .unwrap()
        .id;
    (library.id, root_id)
}

fn insert_media(store: &Store, kind: MediaKind, title: &str) -> MediaId {
    let media = domain::Media {
        id: MediaId::new(),
        kind,
        title: title.into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    media.id
}

fn video_file(
    library_id: &str,
    root_id: &str,
    media_id: MediaId,
    kind: MediaKind,
    path: PathBuf,
) -> LibraryFile {
    LibraryFile {
        file_id: domain::LedgerId::new(),
        library_id: library_id.parse::<LibraryId>().unwrap(),
        root_id: root_id.to_string(),
        media_id,
        kind,
        path,
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(5),
        stat_known: true,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

fn publish(
    store: &Store,
    scope: &WriteScope,
    file: &LibraryFile,
    units: &[UnitKey],
    locale: &str,
) -> Vec<store::metadata::NfoObservation> {
    let observations = store::metadata::observe_nfo_inputs(
        &file.path,
        file.kind,
        units,
        locale,
        &file.version,
        scope.config_revision,
    );
    store
        .publish_files(
            scope,
            &[StorePublication {
                file,
                units,
                expected_version: None,
                nfo_observation: Some(&observations),
                probe_required: false,
            }],
            scope.config_revision,
        )
        .unwrap();
    observations
}

fn overview_of(store: &Store, library: &str, media: MediaId, locale: &str) -> Option<String> {
    store
        .library_item_detail(library.parse().unwrap(), media, locale)
        .unwrap()
        .unwrap()
        .metadata
        .overview
        .value()
        .cloned()
}

/// 请求 locale 必须进查询链：媒体只有 NFO 输入（没有任何已采纳元数据）时，
/// 读 `en-US` 就要拿到 `en-US` 的那行；链（请求 → Library → 实例）之外的语言
/// 即使库里有行也绝不能被隐式选中（`rows.first()` 取语言是禁止的）。
///
/// 新建 Library 的 `display_locale` 用 schema 默认值 `zh-CN`，所以下面两个被观察的
/// locale（`en-US` / `fr-FR`）都不在 Library 兜底上，"读不到" 只能是链没命中。
#[test]
fn nfo_inputs_are_read_at_the_requested_locale_and_never_picked_implicitly() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (library_a, root_a) =
        library_with_root(&store, &tmp.path().join("a"), "A", MediaKind::Movie);
    let (library_b, root_b) =
        library_with_root(&store, &tmp.path().join("b"), "B", MediaKind::Movie);
    let media_a = insert_media(&store, MediaKind::Movie, "A");
    let media_b = insert_media(&store, MediaKind::Movie, "B");

    let file_a = video_file(
        &library_a,
        &root_a,
        media_a,
        MediaKind::Movie,
        tmp.path().join("a/A.mkv"),
    );
    std::fs::write(&file_a.path, b"bytes").unwrap();
    std::fs::write(
        file_a.path.with_extension("nfo"),
        "<movie><plot>Plot A</plot></movie>",
    )
    .unwrap();

    let file_b = video_file(
        &library_b,
        &root_b,
        media_b,
        MediaKind::Movie,
        tmp.path().join("b/B.mkv"),
    );
    std::fs::write(&file_b.path, b"bytes").unwrap();
    std::fs::write(
        file_b.path.with_extension("nfo"),
        "<movie><plot>Plot B</plot></movie>",
    )
    .unwrap();

    let scope_a = WriteScope {
        library_id: library_a.parse().unwrap(),
        config_revision: 0,
    };
    let scope_b = WriteScope {
        library_id: library_b.parse().unwrap(),
        config_revision: 0,
    };
    publish(&store, &scope_a, &file_a, &[UnitKey::WHOLE], "en-US");
    publish(&store, &scope_b, &file_b, &[UnitKey::WHOLE], "fr-FR");

    // 显式请求的语言必须命中，这正是 NFO-only 媒体（没有 media_metadata 行）丢掉请求语言的回归点。
    assert_eq!(
        overview_of(&store, &library_a, media_a, "en-US").as_deref(),
        Some("Plot A")
    );
    assert_eq!(
        overview_of(&store, &library_b, media_b, "fr-FR").as_deref(),
        Some("Plot B")
    );
    // 库里有行、但语言不在链上：不请求它就一个字段都读不到，也不会读成别库的值。
    assert_eq!(overview_of(&store, &library_a, media_a, "fr-FR"), None);
    assert_eq!(overview_of(&store, &library_a, media_a, ""), None);
    assert_eq!(overview_of(&store, &library_b, media_b, "en-US"), None);
}

/// work/season/E01/E02 四层各自观察、各自落行：作品卡片只吃 work 层，兄弟集的发布
/// 既不改写作品层，也不把各自的集层输入串到一起。
#[test]
fn tv_sidecar_layers_are_observed_and_do_not_leak_across_layers() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let shows = tmp.path().join("shows");
    let (library, root_id) = library_with_root(&store, &shows, "Shows", MediaKind::Tv);
    let media = insert_media(&store, MediaKind::Tv, "Show");

    let season_dir = shows.join("Show").join("Season 01");
    std::fs::create_dir_all(&season_dir).unwrap();
    std::fs::write(
        shows.join("Show").join("tvshow.nfo"),
        "<tvshow><title>Work title</title><plot>Work plot</plot><studio>Work Studio</studio></tvshow>",
    )
    .unwrap();
    std::fs::write(
        season_dir.join("season.nfo"),
        "<season><title>Season title</title><plot>Season plot</plot></season>",
    )
    .unwrap();
    let episode_nfo = |number: u32, title: &str, plot: &str, studio: &str| {
        format!(
            "<episodedetails><title>{title}</title><plot>{plot}</plot><studio>{studio}</studio></episodedetails>"
        )
        .replace("<episodedetails>", &format!("<episodedetails><season>1</season><episode>{number}</episode>"))
    };
    let e01 = season_dir.join("Show.S01E01.mkv");
    std::fs::write(&e01, b"e01").unwrap();
    std::fs::write(
        season_dir.join("Show.S01E01.nfo"),
        episode_nfo(1, "Episode one", "Episode one plot", "Episode Studio"),
    )
    .unwrap();

    let file = video_file(&library, &root_id, media, MediaKind::Tv, e01.clone());
    let units = [UnitKey::new(1, 1)];
    let scope = WriteScope {
        library_id: library.parse().unwrap(),
        config_revision: 0,
    };
    let observations = publish(&store, &scope, &file, &units, "zh-CN");

    assert_eq!(observations.len(), 3, "{observations:#?}");
    let scoped = |scope: EntityScope| {
        observations
            .iter()
            .find(|observation| observation.scope == scope)
            .unwrap_or_else(|| panic!("缺少 {scope:?} 的观察结果：{observations:#?}"))
    };
    // input_key 取 sidecar 的规范路径，不是视频路径。
    assert!(
        scoped(EntityScope::Work)
            .input_key
            .ends_with("Show/tvshow.nfo")
    );
    assert!(
        scoped(EntityScope::Season(1))
            .input_key
            .ends_with("Season 01/season.nfo")
    );
    assert!(
        scoped(EntityScope::Episode {
            season: 1,
            episode: 1
        })
        .input_key
        .ends_with("Show.S01E01.nfo")
    );
    for observation in &observations {
        assert_eq!(observation.state, store::metadata::NfoParseState::Valid);
        assert_ne!(observation.input_key, file.path.to_string_lossy());
    }

    // 作品卡片只显示 work 层的值：season/episode 层的 title/plot/studio 都不能漏上来。
    let detail = store
        .library_item_detail(scope.library_id, media, "zh-CN")
        .unwrap()
        .unwrap();
    assert_eq!(
        detail.metadata.title.value().map(String::as_str),
        Some("Work title")
    );
    assert_eq!(
        detail.metadata.overview.value().map(String::as_str),
        Some("Work plot")
    );
    assert_eq!(
        detail.metadata.studios,
        FieldValue::Value(vec!["Work Studio".into()])
    );

    // 兄弟集 E02：自己的集层输入独立成行，作品层保持 E01 发布后的值。
    let e02 = season_dir.join("Show.S01E02.mkv");
    std::fs::write(&e02, b"e02").unwrap();
    std::fs::write(
        season_dir.join("Show.S01E02.nfo"),
        episode_nfo(2, "Episode two", "Episode two plot", "Episode Studio"),
    )
    .unwrap();
    let sibling = video_file(&library, &root_id, media, MediaKind::Tv, e02);
    let sibling_units = [UnitKey::new(1, 2)];
    let sibling_observations = publish(&store, &scope, &sibling, &sibling_units, "zh-CN");
    let sibling_episode = sibling_observations
        .iter()
        .find(|observation| {
            observation.scope
                == EntityScope::Episode {
                    season: 1,
                    episode: 2,
                }
        })
        .expect("E02 必须有自己的集层观察结果");
    assert!(sibling_episode.input_key.ends_with("Show.S01E02.nfo"));
    assert_ne!(
        sibling_episode.input_key,
        scoped(EntityScope::Work).input_key
    );

    let detail = store
        .library_item_detail(scope.library_id, media, "zh-CN")
        .unwrap()
        .unwrap();
    assert_eq!(
        detail.metadata.overview.value().map(String::as_str),
        Some("Work plot")
    );
    assert_eq!(
        detail.metadata.studios,
        FieldValue::Value(vec!["Work Studio".into()])
    );
}

/// NFO `<thumb>` 必须真的落进 episode 层的观测输入并出现在页内事实里，
/// 否则只有 NFO 有剧照的剧集页内会静默丢主图。
/// tvshow 层的 `<thumb>` 是海报语义，不得被映射，也不得被 overlay 漏进集层。
#[test]
fn episode_thumb_lands_in_observed_inputs_and_page_fact_but_work_thumb_does_not_leak() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let shows = tmp.path().join("shows");
    let (library, root_id) = library_with_root(&store, &shows, "Shows", MediaKind::Tv);
    let media = insert_media(&store, MediaKind::Tv, "Show");

    let thumb = "https://image.tmdb.org/t/p/w500/episode-still.jpg";
    let season_dir = shows.join("Show").join("Season 01");
    std::fs::create_dir_all(&season_dir).unwrap();
    std::fs::write(
        shows.join("Show").join("tvshow.nfo"),
        "<tvshow><title>Show</title>\
         <thumb>https://image.tmdb.org/t/p/w500/work-poster.jpg</thumb></tvshow>",
    )
    .unwrap();
    let episode = season_dir.join("Show.S01E01.mkv");
    std::fs::write(&episode, b"e01").unwrap();
    std::fs::write(
        season_dir.join("Show.S01E01.nfo"),
        format!(
            "<episodedetails><season>1</season><episode>1</episode>\
             <title>Pilot</title><thumb>{thumb}</thumb></episodedetails>"
        ),
    )
    .unwrap();

    let file = video_file(&library, &root_id, media, MediaKind::Tv, episode);
    let units = [UnitKey::new(1, 1)];
    let scope = WriteScope {
        library_id: library.parse().unwrap(),
        config_revision: 0,
    };
    publish(&store, &scope, &file, &units, "zh-CN");

    // 落库证据：NFO 的落库表是观测输入 `library_metadata_inputs`（`media_episodes.still_url`
    // 只由 provider 归一化采纳写入，见 `crates/store/src/metadata/adopt.rs` 的 episode 分支；
    // NFO 从不进 adopt，`media_metadata_sources` 也只在 adopt 里写）。所以这里的「真的落了」
    // 必须直接读观测行，而不是读采纳行。
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    let observed = |entity: &str, season: i64, episode: i64| -> domain::MetadataFields {
        let fields_json: String = db
            .query_row(
                "SELECT fields_json FROM library_metadata_inputs
                 WHERE media_id = ?1 AND entity_scope = ?2 AND season = ?3 AND episode = ?4
                   AND locale = 'zh-CN' AND parse_state = 'valid'",
                rusqlite::params![media.to_string(), entity, season, episode],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&fields_json).unwrap()
    };
    assert_eq!(
        observed("episode", 1, 1).still_url,
        FieldValue::Value(thumb.into()),
        "episode 观测行必须带上 NFO <thumb> 得到的 still_url"
    );
    assert_eq!(
        observed("work", -1, -1).still_url,
        FieldValue::Missing,
        "Work 观测行不得把 tvshow 的 <thumb> 当作剧照"
    );

    // 页内读取链拿得到同一个值（Task 11 页内元数据只读已采纳事实）。
    let episode_scope = EntityScope::Episode {
        season: 1,
        episode: 1,
    };
    let page = store
        .protocol_page_metadata(scope.library_id, media, episode_scope, Some("zh-CN"))
        .unwrap();
    assert_eq!(page.still_url, FieldValue::Value(thumb.into()));

    // Work 作用域：tvshow 的 `<thumb>` 不得出现在作品层。
    let work = store
        .protocol_page_metadata(scope.library_id, media, EntityScope::Work, Some("zh-CN"))
        .unwrap();
    assert_eq!(
        work.still_url,
        FieldValue::Missing,
        "Work 作用域的 <thumb> 是海报语义，不得写进 still_url"
    );
}

/// R15: a manual override is part of composition and wins over the NFO input in the detail
/// read; fields neither the NFO nor the override provide come from the adopted snapshot.
#[test]
fn manual_override_wins_over_nfo_input_in_detail() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (library, root) = library_with_root(&store, &tmp.path().join("m"), "M", MediaKind::Movie);
    let media = insert_media(&store, MediaKind::Movie, "Base");
    let file = video_file(
        &library,
        &root,
        media,
        MediaKind::Movie,
        tmp.path().join("m/M.mkv"),
    );
    std::fs::write(&file.path, b"bytes").unwrap();
    std::fs::write(
        file.path.with_extension("nfo"),
        "<movie><plot>NFO PLOT</plot></movie>",
    )
    .unwrap();
    let scope = WriteScope {
        library_id: library.parse().unwrap(),
        config_revision: 0,
    };
    publish(&store, &scope, &file, &[UnitKey::WHOLE], "zh-CN");
    assert_eq!(
        overview_of(&store, &library, media, "zh-CN").as_deref(),
        Some("NFO PLOT")
    );

    let mut fields = domain::MetadataFields::default();
    fields.tagline = FieldValue::Value("PROVIDER TAGLINE".into());
    fields.overview = FieldValue::Value("PROVIDER".into());
    store
        .adopt_metadata(
            &domain::MetadataAdoption {
                media_id: media,
                scope: EntityScope::Work,
                locale: "zh-CN".into(),
                source: "tmdb".into(),
                provider_entity_id: "1".into(),
                provider_work_id: "1".into(),
                fetched_at: 1,
                normalized: domain::NormalizedMetadata {
                    fields,
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
    let mut manual = domain::MetadataFields::default();
    manual.overview = FieldValue::Value("MANUAL".into());
    store
        .set_metadata_overrides(media, EntityScope::Work, "zh-CN", &manual, 0)
        .unwrap();

    let detail = store
        .library_item_detail(library.parse().unwrap(), media, "zh-CN")
        .unwrap()
        .unwrap();
    assert_eq!(
        detail.metadata.overview.value().map(String::as_str),
        Some("MANUAL")
    );
    assert_eq!(
        detail.metadata.tagline.value().map(String::as_str),
        Some("PROVIDER TAGLINE"),
        "detail must not restrict adopted fields to a short whitelist"
    );
}

#[test]
fn unnumbered_tv_file_does_not_create_subscription_coverage() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (library, root) = library_with_root(&store, &tmp.path().join("tv"), "TV", MediaKind::Tv);
    let media = insert_media(&store, MediaKind::Tv, "Unnumbered");
    let file = video_file(
        &library,
        &root,
        media,
        MediaKind::Tv,
        tmp.path().join("tv/Unnumbered.mkv"),
    );
    let scope = WriteScope {
        library_id: library.parse().unwrap(),
        config_revision: 0,
    };
    publish(&store, &scope, &file, &[], "zh-CN");
    let subscribe = domain::Subscribe {
        id: domain::SubscribeId::new(),
        user_id: domain::UserId::new(),
        media_id: media,
        coverage: domain::Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: Some(2),
        },
        fetch_mode: domain::FetchMode::Search,
        filter_id: domain::FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: Some(scope.library_id),
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };

    let facts = store
        .load_library_subscribe_facts(&subscribe, MediaKind::Tv)
        .unwrap();
    assert!(facts.entries().next().is_none());
    assert!(store.get_file_units(file.file_id).unwrap().is_empty());
    let item = store
        .get_library_item(scope.library_id, media)
        .unwrap()
        .unwrap();
    assert_eq!((item.file_count, item.distinct_episode_count), (1, 0));
}

/// R06: an NFO without its own language stays bound to the locale it was imported in.
/// Rescans after a display-locale switch (same bytes or edited) keep that binding; only
/// a sidecar seen for the first time adopts the current display locale.
#[test]
fn imported_nfo_keeps_its_locale_across_display_locale_switch() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (library, root) = library_with_root(&store, &tmp.path().join("m"), "M", MediaKind::Movie);
    let media = insert_media(&store, MediaKind::Movie, "M");
    let file = video_file(
        &library,
        &root,
        media,
        MediaKind::Movie,
        tmp.path().join("m/M.mkv"),
    );
    std::fs::write(&file.path, b"bytes").unwrap();
    let nfo = file.path.with_extension("nfo");
    std::fs::write(&nfo, "<movie><plot>Plot zh</plot></movie>").unwrap();
    let scope = WriteScope {
        library_id: library.parse().unwrap(),
        config_revision: 0,
    };
    publish(&store, &scope, &file, &[UnitKey::WHOLE], "zh-CN");
    let db = tmp.path().join("library.db");
    let locales = || -> Vec<String> {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let mut stmt = conn
            .prepare("SELECT locale FROM library_metadata_inputs WHERE library_id = ?1")
            .unwrap();
        stmt.query_map([&library], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<String>, _>>()
            .unwrap()
    };
    assert_eq!(locales(), vec!["zh-CN"], "the initial import is real");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE libraries SET display_locale = 'en-US' WHERE library_id = ?1",
            [&library],
        )
        .unwrap();

    store.reconcile_nfo_inputs(&file.path).unwrap();
    assert_eq!(
        locales(),
        vec!["zh-CN"],
        "same-bytes rescan keeps the import locale"
    );
    std::fs::write(&nfo, "<movie><plot>Plot zh edited</plot></movie>").unwrap();
    store.reconcile_nfo_inputs(&file.path).unwrap();
    assert_eq!(locales(), vec!["zh-CN"], "an edit keeps the import locale");
    drop(store);
    let store = Store::open(tmp.path()).unwrap();
    store.reconcile_nfo_inputs(&file.path).unwrap();
    assert_eq!(locales(), vec!["zh-CN"], "restart keeps the import locale");
    assert_eq!(
        overview_of(&store, &library, media, "zh-CN").as_deref(),
        Some("Plot zh edited")
    );

    std::fs::remove_file(&nfo).unwrap();
    store.reconcile_nfo_inputs(&file.path).unwrap();
    assert!(locales().is_empty(), "a removed sidecar releases its input");
    std::fs::write(&nfo, "<movie><plot>Plot en</plot></movie>").unwrap();
    store.reconcile_nfo_inputs(&file.path).unwrap();
    assert_eq!(
        locales(),
        vec!["en-US"],
        "a new sidecar adopts the current locale"
    );
}
