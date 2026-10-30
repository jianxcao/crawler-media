#[path = "management/common.rs"]
mod common;

use std::collections::HashMap;
use std::sync::Arc;

use domain::{Media, MediaId, MediaKind};
use parking_lot::Mutex;

struct NamesCatalog {
    hits: Vec<media::CatalogHit>,
    details: HashMap<String, Media>,
}

struct OfflineArtwork;
impl api::PosterFetch for OfflineArtwork {
    fn get(&self, _: &str) -> Result<Vec<u8>, String> {
        Ok(b"{}".to_vec())
    }
}

impl api::catalog::Catalog for NamesCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(self.hits.clone())
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
    fn details(&self, _: MediaKind, id: &str) -> Result<Option<Media>, String> {
        Ok(self.details.get(id).cloned())
    }
}

fn movie(title: &str, id: Option<&str>) -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.into(),
        year: Some(2023),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: id.map(str::to_string),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn hit(media: Media) -> media::CatalogHit {
    media::CatalogHit {
        media,
        poster_path: None,
        backdrop_path: None,
        rating: None,
        overview: None,
    }
}

fn resolve(wanted: &Media, hits: Vec<media::CatalogHit>) -> Option<Media> {
    resolve_with_details(wanted, hits, HashMap::new())
}

fn resolve_with_details(
    wanted: &Media,
    hits: Vec<media::CatalogHit>,
    details: HashMap<String, Media>,
) -> Option<Media> {
    let tmp = tempfile::tempdir().unwrap();
    let state = common::state(
        tmp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .with_catalog(Arc::new(NamesCatalog { hits, details }))
    .with_poster_fetch(Arc::new(OfflineArtwork));
    state.store().insert_media(wanted).unwrap();
    api::auto_resolve::auto_resolve_media(&state, wanted, &tmp.path().join("movie.mkv"))
}

#[test]
fn localized_alias_from_details_identifies_an_english_search_candidate() {
    let wanted = movie("大侦探皮卡丘", None);
    let candidate = movie("Pokémon Detective Pikachu", Some("447404"));
    let mut details = candidate.clone();
    details.aliases = vec![wanted.title.clone()];
    assert_eq!(
        resolve_with_details(
            &wanted,
            vec![hit(candidate)],
            HashMap::from([("447404".into(), details)])
        )
        .unwrap()
        .tmdb_id
        .as_deref(),
        Some("447404")
    );
}

#[test]
fn localized_search_matches_original_name_and_known_aliases() {
    let wanted = movie("长安三万里", None);
    let mut catalog = movie("Chang'an", Some("1008042"));
    catalog.original_title = Some("长安三万里".into());
    assert_eq!(
        resolve(&wanted, vec![hit(catalog)])
            .unwrap()
            .tmdb_id
            .as_deref(),
        Some("1008042")
    );

    let wanted = movie("哆啦A梦：大雄的绘画奇遇记", None);
    let mut catalog = movie("Doraemon the Movie: Nobita's Art World Tales", Some("123"));
    catalog.aliases = vec![wanted.title.clone()];
    assert_eq!(
        resolve(&wanted, vec![hit(catalog)])
            .unwrap()
            .tmdb_id
            .as_deref(),
        Some("123")
    );
}

#[test]
fn ambiguous_exact_names_never_bind_the_first_catalog_row() {
    let wanted = movie("Common Name", None);
    assert!(
        resolve(
            &wanted,
            vec![
                hit(movie("Common Name", Some("one"))),
                hit(movie("Common Name", Some("two")))
            ]
        )
        .is_none()
    );
}

#[test]
fn matching_original_name_still_rejects_wrong_year() {
    let wanted = movie("长安三万里", None);
    let mut catalog = movie("Chang'an", Some("1008042"));
    catalog.original_title = Some(wanted.title.clone());
    catalog.year = Some(1993);
    assert!(resolve(&wanted, vec![hit(catalog)]).is_none());
}
