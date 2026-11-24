//! today-arrivals：先在 Store 锁内取 owned 快照，再在 blocking 预算内做
//! stat + catalog 查询 + 渲染。旧实现全程持锁并在锁内发网络请求，而
//! `TmdbHttp` 需要同一个 Store 读取配置 → 锁重入 → 冷缓存时预测整体消失。

use std::collections::HashMap;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::blocking_budget::BlockingError;
use crate::http::subscriptions::{coverage_json, parse_upload_time};
use crate::http::{err, ok_list};
use crate::management::ApiState;

use super::forecast::ForecastBasis;

/// 锁内读到的 owned 快照；此后不再需要 Store 锁。
pub(crate) struct ArrivalSnapshot {
    pub(crate) subscribe: domain::Subscribe,
    pub(crate) media: domain::Media,
    pub(crate) pending: Vec<(i32, crate::store::PendingDownload)>,
    pub(crate) facts: domain::SubscribeFacts,
    pub(crate) rows: Vec<domain::FileCoverage>,
    pub(crate) episodes: Vec<media::SeasonEpisode>,
}

fn resolve_effective_target_library(
    store: &crate::Store,
    subscribe: &domain::Subscribe,
    media_kind: domain::MediaKind,
    user_id: domain::UserId,
    admin: bool,
) -> Result<Option<domain::LibraryId>, crate::store::StoreError> {
    match subscribe.library_id {
        Some(target_id) => {
            let id_str = target_id.to_string();
            let Some(library) = store.get_library(&id_str)? else {
                return Ok(None);
            };
            if library.kind != media_kind
                || !store.library_accepts_work(&id_str)?
                || !crate::http::routing_preview::user_can_target_library(
                    &library,
                    Some(user_id),
                    admin,
                )
            {
                return Ok(None);
            }
            Ok(Some(target_id))
        }
        None => {
            let Some(default_lib) = store.default_library(media_kind)? else {
                return Ok(None);
            };
            if !store.library_accepts_work(&default_lib.id)?
                || !crate::http::routing_preview::user_can_auto_route_library(
                    &default_lib,
                    Some(user_id),
                    admin,
                )
            {
                return Ok(None);
            }
            Ok(default_lib.id.parse().ok())
        }
    }
}

/// 只在持有 Store 锁时读取/Clone；DB 错误向上传播（handler 记 500，不静默空集合）。
pub(crate) fn load_arrivals(
    store: &crate::Store,
    user_id: domain::UserId,
) -> Result<Vec<ArrivalSnapshot>, crate::store::StoreError> {
    // 用户隔离：成员只看自己订阅的到货/预计入库；admin 看全部。
    let admin = store.user_role(user_id)? == "admin";
    let subscribes = if admin {
        store.list_all_subscribes()?
    } else {
        store.list_subscribes_for_user(user_id)?
    };
    let mut resolved = Vec::with_capacity(subscribes.len());
    let mut coverage_media_ids = Vec::new();
    for subscribe in subscribes {
        let Some(media) = store.get_media(subscribe.media_id)? else {
            continue;
        };
        let target_lib =
            resolve_effective_target_library(store, &subscribe, media.kind, user_id, admin)?;
        if target_lib.is_some() {
            coverage_media_ids.push(subscribe.media_id);
        }
        resolved.push((subscribe, media, target_lib));
    }
    let mut rows_by_media: HashMap<domain::MediaId, Vec<domain::FileCoverage>> = HashMap::new();
    if !coverage_media_ids.is_empty() {
        for coverage in store.file_coverage_for_media_batch(&coverage_media_ids)? {
            if coverage.file.publication != domain::FilePublication::Published
                || coverage.file.presence == domain::FilePresence::Missing
            {
                continue;
            }
            rows_by_media
                .entry(coverage.file.media_id)
                .or_default()
                .push(coverage);
        }
    }
    let mut snapshots = Vec::with_capacity(resolved.len());
    for (subscribe, media, target_lib) in resolved {
        let pending = store.load_pending(subscribe.id)?;
        let facts = if media.kind == domain::MediaKind::Tv {
            store.load_subscribe_facts(subscribe.id)?
        } else {
            domain::SubscribeFacts::default()
        };
        let rows = match target_lib {
            Some(target_lib) => rows_by_media
                .get(&subscribe.media_id)
                .map(|rows| {
                    rows.iter()
                        .filter(|coverage| coverage.file.library_id == target_lib)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let episodes = if let Some(target_lib) = target_lib {
            let mut scoped = subscribe.clone();
            scoped.library_id = Some(target_lib);
            super::catalog_facts::adopted_episodes(store, &scoped, &rows)?
        } else {
            Vec::new()
        };
        snapshots.push(ArrivalSnapshot {
            episodes,
            subscribe,
            media,
            pending,
            facts,
            rows,
        });
    }
    Ok(snapshots)
}

/// 锁外计算：stat、catalog 查询与 JSON 渲染。同 (tmdb_id, season) 每请求只查一次。
pub(crate) fn build_arrivals(
    snapshots: Vec<ArrivalSnapshot>,
    catalog: &dyn crate::catalog::Catalog,
    now: i64,
) -> Vec<Value> {
    let day_secs = 86_400i64;
    let mut items = Vec::new();
    let mut episodes_cache: HashMap<(String, u32), Vec<media::SeasonEpisode>> = HashMap::new();
    for snapshot in snapshots {
        let ArrivalSnapshot {
            subscribe,
            media,
            pending,
            facts,
            rows,
            episodes,
        } = snapshot;
        let forecast_basis = ForecastBasis::new(&pending);
        let cov_json = coverage_json(&subscribe.coverage);
        let grabbed_eps = push_grabbed_arrivals(
            &mut items,
            &subscribe,
            &media.title,
            &cov_json,
            &pending,
            &facts,
        );
        let imported_units = push_imported_arrivals(
            &mut items,
            &subscribe,
            &media,
            &cov_json,
            &rows,
            pending.is_empty(),
            now,
            day_secs,
        );
        // 预计发布（wanted）：TV 订阅中 air_date 落在 [昨天, +7 天] 且未入库/未在途的单元。
        if media.kind != domain::MediaKind::Tv {
            continue;
        }
        let season = match subscribe.coverage {
            domain::Coverage::Tv { season, .. } => season,
            _ => continue,
        };
        let mut available: std::collections::BTreeMap<u32, media::SeasonEpisode> = media
            .tmdb_id
            .as_deref()
            .map(|tmdb_id| episodes_for(catalog, &mut episodes_cache, tmdb_id, season))
            .unwrap_or_default()
            .iter()
            .cloned()
            .map(|episode| (episode.episode_number, episode))
            .collect();
        available.extend(
            episodes
                .into_iter()
                .map(|episode| (episode.episode_number, episode)),
        );
        for episode in available.into_values() {
            if facts
                .get(Some(season), Some(episode.episode_number))
                .is_some()
                || grabbed_eps.contains(&(season, episode.episode_number))
                || imported_units.contains(&(season, episode.episode_number))
            {
                continue;
            }
            let Some(air_date) = episode.air_date.as_deref() else {
                continue;
            };
            let Some(air_unix) = parse_upload_time(air_date) else {
                continue;
            };
            if air_unix < now - day_secs || air_unix > now + 7 * day_secs {
                continue;
            }
            let forecast = forecast_basis.for_date(air_date).unwrap_or(Value::Null);
            items.push(json!({
                "subscription_id": subscribe.id.to_string(),
                "title": media.title,
                "coverage": cov_json,
                "eta": null,
                "status": "wanted",
                "air_date": air_date,
                "season": season,
                "episode": episode.episode_number,
                "info_hash": Value::Null,
                "grabbed_at": Value::Null,
                "release_forecast": forecast,
            }));
        }
    }
    items
}

fn push_grabbed_arrivals(
    items: &mut Vec<Value>,
    subscribe: &domain::Subscribe,
    title: &str,
    cov_json: &Value,
    pending: &[(i32, crate::store::PendingDownload)],
    facts: &domain::SubscribeFacts,
) -> std::collections::BTreeSet<(u32, u32)> {
    let mut grabbed_eps = std::collections::BTreeSet::new();
    let default_season = match subscribe.coverage {
        domain::Coverage::Tv { season, .. } => Some(season),
        _ => None,
    };
    for (_, pd) in pending {
        let parsed = pd
            .release_override
            .clone()
            .unwrap_or_else(|| release::parse(&pd.torrent.title));
        let grabbed_at = pd.submitted_at.map(crate::store::rfc3339_from_secs);
        let season = if parsed.season_inferred {
            default_season.or(parsed.season)
        } else {
            parsed.season.or(default_season)
        };
        if let (Some(s), Some(ep_from)) = (season, parsed.episode) {
            let ep_to = parsed
                .episode_to
                .unwrap_or(ep_from)
                .max(ep_from)
                .min(ep_from + 60);
            for ep in ep_from..=ep_to {
                if facts.get(Some(s), Some(ep)).is_some() {
                    continue;
                }
                grabbed_eps.insert((s, ep));
                items.push(json!({
                    "subscription_id": subscribe.id.to_string(),
                    "title": title,
                    "coverage": cov_json,
                    "eta": null,
                    "status": "grabbed",
                    "season": s,
                    "episode": ep,
                    "info_hash": Value::Null,
                    "grabbed_at": grabbed_at,
                }));
            }
        } else {
            items.push(json!({
                "subscription_id": subscribe.id.to_string(),
                "title": title,
                "coverage": cov_json,
                "eta": null,
                "status": "grabbed",
                "season": season,
                "info_hash": Value::Null,
                "grabbed_at": grabbed_at,
            }));
        }
    }
    grabbed_eps
}

fn push_imported_arrivals(
    items: &mut Vec<Value>,
    subscribe: &domain::Subscribe,
    media: &domain::Media,
    cov_json: &Value,
    rows: &[domain::FileCoverage],
    pending_empty: bool,
    now: i64,
    day_secs: i64,
) -> std::collections::BTreeSet<(u32, u32)> {
    let today_date = crate::store::rfc3339_from_secs(now)
        .get(..10)
        .unwrap_or("")
        .to_string();
    if media.kind == domain::MediaKind::Tv {
        let target_season = match subscribe.coverage {
            domain::Coverage::Tv { season, .. } => Some(season),
            _ => None,
        };
        let mut owned_units = std::collections::BTreeSet::new();
        let mut imported_units = std::collections::BTreeSet::new();
        for coverage in rows {
            let recent = modified_within_day(&coverage.file.path, now, day_secs);
            for unit in &coverage.units {
                if let Some((s, ep)) = unit.episode_coordinates()
                    && target_season.is_none_or(|ts| ts == s)
                {
                    owned_units.insert((s, ep));
                    if recent {
                        imported_units.insert((s, ep));
                    }
                }
            }
        }
        for (s, ep) in imported_units {
            items.push(json!({
                "subscription_id": subscribe.id.to_string(),
                "title": media.title,
                "coverage": cov_json,
                "eta": null,
                "status": "imported",
                "air_date": today_date,
                "season": s,
                "episode": ep,
                "info_hash": Value::Null,
                "grabbed_at": Value::Null,
            }));
        }
        return owned_units;
    }
    let imported_today = rows
        .iter()
        .any(|coverage| modified_within_day(&coverage.file.path, now, day_secs));
    if imported_today && pending_empty {
        items.push(json!({
            "subscription_id": subscribe.id.to_string(),
            "title": media.title,
            "coverage": cov_json,
            "eta": null,
            "status": "imported",
            "air_date": today_date,
            "info_hash": Value::Null,
            "grabbed_at": Value::Null,
        }));
    }
    std::collections::BTreeSet::new()
}

/// 部分数据源失败只影响该 (tmdb_id, season)：warn 后跳过，保留其余 items。
fn episodes_for<'a>(
    catalog: &dyn crate::catalog::Catalog,
    cache: &'a mut HashMap<(String, u32), Vec<media::SeasonEpisode>>,
    tmdb_id: &str,
    season: u32,
) -> &'a [media::SeasonEpisode] {
    cache
        .entry((tmdb_id.to_string(), season))
        .or_insert_with(|| match catalog.cached_season_episodes(tmdb_id, season) {
            Ok(episodes) => episodes.unwrap_or_default(),
            Err(error) => {
                tracing::warn!(
                    %error,
                    tmdb_id,
                    season,
                    "today-arrivals 无法读取季度集数，跳过该季预测"
                );
                Vec::new()
            }
        })
        .as_slice()
}

fn modified_within_day(path: &std::path::Path, now: i64, day_secs: i64) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map(|modified| {
            modified
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs() as i64)
                .unwrap_or(0)
                >= now - day_secs
        })
        .unwrap_or(false)
}

pub(crate) async fn today_arrivals(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
) -> Response {
    let snapshots = match crate::http::store_wait::store_read(
        &state,
        &[store::Db::Library, store::Db::App, store::Db::Subscribe],
        move |store| load_arrivals(store, user_id),
    )
    .await
    {
        Ok(snapshots) => snapshots,
        Err(error) => return error.response(),
    };
    let snapshots = match snapshots {
        Ok(snapshots) => snapshots,
        Err(error) => {
            tracing::error!(%error, %user_id, "today-arrivals 读取快照失败");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let catalog = state.catalog.clone();
    let now = crate::job_loop::unix_now();
    // 30s 是总计算等待预算，不替代 CatalogGet 自身的 HTTP 超时。
    let work = state
        .http_work
        .run(std::time::Duration::from_secs(30), move || {
            build_arrivals(snapshots, catalog.as_ref(), now)
        })
        .await;
    match work {
        Ok(items) => ok_list(items).into_response(),
        Err(BlockingError::Busy) => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请重试",
        ),
        Err(BlockingError::Timeout) => err(
            StatusCode::GATEWAY_TIMEOUT,
            "arrivals.timeout",
            "预测计算超时",
        ),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "arrivals.worker",
            &error.to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{
        Coverage, FetchMode, FilterId, Media, MediaId, MediaKind, SiteId, Subscribe,
        SubscribeFacts, SubscribeId, Torrent, UserId,
    };

    struct StubSeason2Catalog;
    impl crate::catalog::Catalog for StubSeason2Catalog {
        fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
            Ok(Vec::new())
        }
        fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
            Ok(Vec::new())
        }
        fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
            Ok(Vec::new())
        }
        fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
            Ok(Vec::new())
        }
        fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
            Ok(None)
        }
        fn cached_season_episodes(
            &self,
            _: &str,
            season: u32,
        ) -> Result<Option<Vec<media::SeasonEpisode>>, String> {
            if season != 2 {
                return Ok(None);
            }
            Ok(Some(vec![media::SeasonEpisode {
                episode_number: 3,
                air_date: Some("2026-01-15".into()),
            }]))
        }
    }

    #[test]
    fn season_inferred_pending_release_uses_subscription_season_in_today_arrivals() {
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "葬送的芙莉莲".into(),
            year: Some(2026),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("209867".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        let subscribe = Subscribe {
            id: SubscribeId::new(),
            user_id: UserId::new(),
            media_id: media.id,
            coverage: Coverage::Tv {
                season: 2,
                episode_from: 1,
                episode_to: None,
            },
            fetch_mode: FetchMode::Both,
            filter_id: FilterId::new(),
            wash_cut: false,
            wash_cut_filter_id: None,
            keep_old_versions: false,
            full_season_pack: false,
            downloader_id: None,
            library_id: None,
            tracking_state: "active".into(),
            follow_future: true,
            search_interval_secs: 1800,
        };
        let raw_title = "Sousou.no.Frieren.EP03.1080p.WEB-DL.mkv";
        let check_parsed = release::parse(raw_title);
        assert_eq!(check_parsed.season, Some(1));
        assert_eq!(check_parsed.episode, Some(3));
        assert!(check_parsed.season_inferred);
        let pending = vec![(
            1,
            crate::store::PendingDownload {
                torrent: Torrent {
                    site_id: SiteId::new(),
                    title: raw_title.into(),
                    enclosure: "https://example.invalid/03.torrent".into(),
                    size_bytes: None,
                    seeders: None,
                    free: false,
                    hr: None,
                    imdb_id: None,
                    id: None,
                    leechers: None,
                    snatched: None,
                    upload_time: None,
                    detail_url: None,
                    category: None,
                    poster_url: None,
                },
                release_override: None,
                downloader_id: None,
                submitted_at: Some(1_768_435_200),
            },
        )];
        let now = parse_upload_time("2026-01-15").unwrap();
        let items = build_arrivals(
            vec![ArrivalSnapshot {
                subscribe,
                media,
                pending,
                facts: SubscribeFacts::default(),
                rows: Vec::new(),
                episodes: Vec::new(),
            }],
            &StubSeason2Catalog,
            now,
        );
        assert_eq!(
            items.len(),
            1,
            "grabbed S02E03 must suppress duplicate wanted item: {items:?}"
        );
        assert_eq!(items[0]["status"], "grabbed");
        assert_eq!(items[0]["season"], 2);
        assert_eq!(items[0]["episode"], 3);
    }

    fn seed_arrivals_store(
        tmp: &tempfile::TempDir,
    ) -> (crate::Store, UserId, UserId, FilterId, Media) {
        let store = crate::Store::open(tmp.path().join("data")).unwrap();
        let admin_id = UserId::new();
        let member_id = UserId::new();
        for (id, login, role) in [
            (admin_id, "admin", domain::UserRole::Admin),
            (member_id, "member", domain::UserRole::Member),
        ] {
            store
                .insert_user_with_password(
                    &domain::User {
                        id,
                        login: login.into(),
                        enabled: true,
                        role,
                    },
                    "secret",
                )
                .unwrap();
        }
        let filter = domain::Filter::new(FilterId::new(), "default", Vec::new());
        store.insert_filter(&filter).unwrap();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "葬送的芙莉莲".into(),
            year: Some(2026),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("209867".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        (store, admin_id, member_id, filter.id, media)
    }

    fn publish_s2e3(store: &crate::Store, media_id: MediaId, dir: &std::path::Path) {
        let path = dir.join("S02E03.mkv");
        std::fs::write(&path, b"video").unwrap();
        store::test_support::publish_fixture_file(
            store,
            &store::test_support::FixtureFile {
                file_id: Some(domain::LedgerId::new()),
                media_id,
                path: path.display().to_string(),
                season: Some(2),
                episode: Some(3),
                ..Default::default()
            },
        )
        .unwrap();
    }

    fn insert_s2_subscribe(
        store: &crate::Store,
        user_id: UserId,
        media_id: MediaId,
        filter_id: FilterId,
        library_id: Option<domain::LibraryId>,
    ) {
        store
            .insert_subscribe(&Subscribe {
                id: SubscribeId::new(),
                user_id,
                media_id,
                coverage: Coverage::Tv {
                    season: 2,
                    episode_from: 1,
                    episode_to: None,
                },
                fetch_mode: FetchMode::Search,
                filter_id,
                wash_cut: false,
                wash_cut_filter_id: None,
                keep_old_versions: false,
                full_season_pack: false,
                downloader_id: None,
                library_id,
                tracking_state: "active".into(),
                follow_future: true,
                search_interval_secs: 1800,
            })
            .unwrap();
    }

    #[test]
    fn untargeted_subscription_ignores_non_default_and_hidden_default_libraries() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, admin_id, _, filter_id, media) = seed_arrivals_store(&tmp);
        let default_root = tmp.path().join("tv-default");
        let other_root = tmp.path().join("tv-other");
        std::fs::create_dir_all(&default_root).unwrap();
        std::fs::create_dir_all(&other_root).unwrap();

        let default_lib = store
            .create_library(
                MediaKind::Tv,
                "默认剧集",
                &[default_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        store.set_default_library(&default_lib.id).unwrap();
        store
            .create_library(
                MediaKind::Tv,
                "其他剧集",
                &[other_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        insert_s2_subscribe(&store, admin_id, media.id, filter_id, None);

        publish_s2e3(&store, media.id, &other_root);

        let snaps = load_arrivals(&store, admin_id).unwrap();
        assert!(
            snaps[0].rows.is_empty(),
            "None subscription must not inherit coverage from non-default library"
        );

        publish_s2e3(&store, media.id, &default_root);
        let snaps_default = load_arrivals(&store, admin_id).unwrap();
        assert_eq!(snaps_default[0].rows.len(), 1);
        assert_eq!(
            snaps_default[0].rows[0].file.library_id.to_string(),
            default_lib.id
        );

        store
            .update_library(&default_lib.id, None, None, Some(("everyone", false, &[])))
            .unwrap();
        let snaps_hidden = load_arrivals(&store, admin_id).unwrap();
        assert!(
            snaps_hidden[0].rows.is_empty(),
            "None subscription must not auto-route to admin_visible=false default library"
        );
    }

    #[test]
    fn explicit_subscription_sees_admin_hidden_library_but_enforces_acl_and_accepts_work() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, admin_id, member_id, filter_id, media) = seed_arrivals_store(&tmp);
        let hidden_root = tmp.path().join("tv-hidden");
        std::fs::create_dir_all(&hidden_root).unwrap();
        let hidden_lib = store
            .create_library(
                MediaKind::Tv,
                "隐藏专库",
                &[hidden_root.to_str().unwrap()],
                "selected",
                false,
                &[],
            )
            .unwrap();
        let hidden_lib_id: domain::LibraryId = hidden_lib.id.parse().unwrap();

        publish_s2e3(&store, media.id, &hidden_root);

        insert_s2_subscribe(&store, admin_id, media.id, filter_id, Some(hidden_lib_id));
        insert_s2_subscribe(&store, member_id, media.id, filter_id, Some(hidden_lib_id));

        let admin_snaps = load_arrivals(&store, admin_id).unwrap();
        assert_eq!(admin_snaps[0].rows.len(), 1);
        let now = crate::job_loop::unix_now();
        let items = build_arrivals(admin_snaps, &StubSeason2Catalog, now);
        assert!(
            items
                .iter()
                .any(|i| i["status"] == "imported" && i["episode"] == 3)
        );
        assert!(load_arrivals(&store, member_id).unwrap()[0].rows.is_empty());
        store.begin_library_removal(&hidden_lib.id).unwrap();
        assert!(load_arrivals(&store, admin_id).unwrap()[0].rows.is_empty());
    }
}
