//! Fixtures shared by the subscription-depth scenarios: an authorized app, a fake
//! catalog, a seeded pending Torrent and a fake douban endpoint.

pub(crate) use std::collections::HashMap;
pub(crate) use std::sync::Arc;

pub(crate) use api::catalog::Catalog;
pub(crate) use api::{Store, router};
pub(crate) use axum::http::StatusCode;
pub(crate) use downloader::MemoryDownloader;
pub(crate) use parking_lot::Mutex;
pub(crate) use serde_json::{Value, json};
pub(crate) use tower::ServiceExt;

pub(crate) use super::super::common::*;

pub(crate) fn authed_app(tmp: &tempfile::TempDir) -> axum::Router {
    router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ))
}

/// Fake catalog for title-preview: one movie + a couple of ambiguous hits.
pub(crate) struct FakeCatalog {
    pub(super) movie: domain::Media,
    pub(super) shows: Vec<domain::Media>,
    /// Every catalog call; strict title_ref rejection must leave it at zero.
    pub(super) calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl FakeCatalog {
    fn hit(&self) {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Catalog for FakeCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        self.hit();
        Ok(vec![media::CatalogHit {
            media: self.movie.clone(),
            poster_path: None,
            backdrop_path: None,
            rating: None,
            overview: None,
        }])
    }
    fn search_tv(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        self.hit();
        Ok(self
            .shows
            .iter()
            .map(|media| media::CatalogHit {
                media: media.clone(),
                poster_path: None,
                backdrop_path: None,
                rating: None,
                overview: None,
            })
            .collect())
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        self.hit();
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        self.hit();
        Ok(Vec::new())
    }
    fn details(
        &self,
        kind: domain::MediaKind,
        tmdb_id: &str,
    ) -> Result<Option<domain::Media>, String> {
        self.hit();
        if kind == domain::MediaKind::Movie && self.movie.tmdb_id.as_deref() == Some(tmdb_id) {
            Ok(Some(self.movie.clone()))
        } else {
            Ok(self
                .shows
                .iter()
                .find(|m| m.tmdb_id.as_deref() == Some(tmdb_id))
                .cloned())
        }
    }
    fn tv_seasons(&self, _tmdb_id: &str) -> Result<Vec<media::TvSeason>, String> {
        self.hit();
        Ok(vec![media::TvSeason {
            season_number: 1,
            name: "Season 1".into(),
            episode_count: Some(8),
            air_date: Some("2024-01-01".into()),
            overview: None,
            poster_path: None,
        }])
    }
    fn season_episodes(
        &self,
        _tmdb_id: &str,
        _season: u32,
    ) -> Result<Vec<media::SeasonEpisode>, String> {
        self.hit();
        Ok((1..=4)
            .map(|episode_number| media::SeasonEpisode {
                episode_number,
                air_date: Some(format!("2024-01-{episode_number:02}")),
            })
            .collect())
    }
}

pub(crate) fn fake_movie() -> domain::Media {
    domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "The Matrix".into(),
        year: Some(1999),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

pub(crate) fn fake_state(tmp: &tempfile::TempDir) -> (axum::Router, Arc<MemoryDownloader>) {
    let (app, downloader, _calls) = fake_state_counting(tmp);
    (app, downloader)
}

/// [`fake_state`] plus the catalog call counter.
pub(crate) fn fake_state_counting(
    tmp: &tempfile::TempDir,
) -> (
    axum::Router,
    Arc<MemoryDownloader>,
    Arc<std::sync::atomic::AtomicUsize>,
) {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let catalog = Arc::new(FakeCatalog {
        calls: calls.clone(),
        movie: fake_movie(),
        shows: vec![
            domain::Media {
                id: domain::MediaId::new(),
                kind: domain::MediaKind::Tv,
                title: "The Matrix Resurrections".into(),
                year: Some(2021),
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: Some("624860".into()),
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            },
            domain::Media {
                id: domain::MediaId::new(),
                kind: domain::MediaKind::Tv,
                title: "The Matrix Reimagined".into(),
                year: None,
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: Some("999".into()),
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            },
        ],
    });
    let state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader.clone(),
    )
    .with_catalog(catalog);
    (router(state), downloader, calls)
}

pub(crate) fn seed_pending_torrent(
    tmp: &tempfile::TempDir,
    subscribe_id: &str,
    title: &str,
    enclosure: &str,
) {
    let store = Store::open(tmp.path().join("data")).unwrap();
    let torrent = domain::Torrent {
        site_id: domain::SiteId::new(),
        title: title.into(),
        enclosure: enclosure.into(),
        size_bytes: Some(1024 * 1024),
        seeders: Some(5),
        free: false,
        hr: false,
        imdb_id: None,
        id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    };
    store
        .merge_pending(
            subscribe_id.parse().unwrap(),
            &[(
                80,
                api::store::PendingDownload {
                    torrent,
                    release_override: None,
                    downloader_id: None,
                    submitted_at: None,
                },
            )],
        )
        .unwrap();
}

pub(crate) struct FakeDoubanHttp;

impl media::CatalogGet for FakeDoubanHttp {
    fn get(&self, _path: &str) -> Result<String, media::TmdbError> {
        Ok(douban_html::movie().to_string())
    }
}

pub(crate) mod douban_html {
    pub fn movie() -> &'static str {
        r#"<html><body>
            <h1><span property="v:itemreviewed">肖申克的救赎</span> <span class="year">(1994)</span></h1>
            <div id="info"><span class="pl">类型:</span> 剧情 / 犯罪</div>
        </body></html>"#
    }
}

pub(crate) fn douban_app(tmp: &tempfile::TempDir) -> axum::Router {
    let douban = api::catalog::DoubanCatalog::new(
        media::Douban::new(FakeDoubanHttp, &tmp.path().join("catalog.db")).unwrap(),
    );
    let fanout = api::catalog::FanoutCatalog::new(vec![douban]);
    router(
        state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .with_catalog(fanout),
    )
}
