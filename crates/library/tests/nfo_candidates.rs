use domain::{EntityScope, MediaKind, UnitKey};
use library::nfo::{NfoHeader, NfoLayer, nfo_sidecars, parse_nfo_header, resolve_nfo_scope};
use std::path::{Path, PathBuf};

fn header(body: &str) -> NfoHeader {
    parse_nfo_header(body).expect("header")
}

fn sidecar(layer: NfoLayer, path: &Path, kind: MediaKind) -> PathBuf {
    nfo_sidecars(path, kind)
        .into_iter()
        .find(|candidate| candidate.layer == layer)
        .unwrap_or_else(|| panic!("{layer:?} sidecar for {}", path.display()))
        .path
}

#[test]
fn tv_sidecars_cover_work_season_and_episode() {
    let path = Path::new("/lib/Show/Season 2/Show - S02E04.mkv");
    let episode = sidecar(NfoLayer::Episode, path, MediaKind::Tv);
    let season = sidecar(NfoLayer::Season, path, MediaKind::Tv);
    let work = sidecar(NfoLayer::Work, path, MediaKind::Tv);
    assert_eq!(
        episode,
        PathBuf::from("/lib/Show/Season 2/Show - S02E04.nfo")
    );
    assert_eq!(season, PathBuf::from("/lib/Show/Season 2/season.nfo"));
    assert_eq!(work, PathBuf::from("/lib/Show/tvshow.nfo"));
}

#[test]
fn movie_sidecars_stay_work_scoped() {
    let path = Path::new("/lib/Movies/The Matrix (1999).mkv");
    let sidecars = nfo_sidecars(path, MediaKind::Movie);
    assert!(
        sidecars
            .iter()
            .all(|candidate| candidate.layer == NfoLayer::Work)
    );
    assert_eq!(
        sidecars[0].path,
        PathBuf::from("/lib/Movies/The Matrix (1999).nfo")
    );
    assert!(
        sidecars
            .iter()
            .any(|candidate| candidate.path == *"/lib/Movies/movie.nfo")
    );
}

#[test]
fn episode_sidecar_uses_xml_coordinates_when_the_name_has_none() {
    let path = Path::new("/lib/Show/Season 2/Show.mkv");
    let coords = header("<episodedetails><season>2</season><episode>4</episode></episodedetails>");
    assert_eq!(
        resolve_nfo_scope(NfoLayer::Episode, path, &[UnitKey::WHOLE], &coords),
        Some(EntityScope::Episode {
            season: 2,
            episode: 4
        })
    );
}

#[test]
fn episode_sidecar_coordinates_must_match_known_coverage() {
    let path = Path::new("/lib/Show/Season 1/Show.S01E03.mkv");
    let coords = header("<episodedetails><season>2</season><episode>4</episode></episodedetails>");
    assert_eq!(
        resolve_nfo_scope(NfoLayer::Episode, path, &[UnitKey::new(1, 3)], &coords),
        None,
        "a sidecar that disagrees with the ledger coverage must not be adopted"
    );
}

#[test]
fn episode_sidecar_without_coordinates_needs_a_single_known_unit() {
    let path = Path::new("/lib/Show/Season 1/Show.S01E03.mkv");
    let coords = header("<episodedetails><title>Episode</title></episodedetails>");
    assert_eq!(
        resolve_nfo_scope(NfoLayer::Episode, path, &[UnitKey::new(1, 3)], &coords),
        Some(EntityScope::Episode {
            season: 1,
            episode: 3
        })
    );
    assert_eq!(
        resolve_nfo_scope(
            NfoLayer::Episode,
            path,
            &[UnitKey::new(1, 3), UnitKey::new(1, 4)],
            &coords
        ),
        None
    );
}

#[test]
fn season_sidecar_comes_from_the_directory_name() {
    let path = Path::new("/lib/Show/Season 2/Show.S02E04.mkv");
    let coords = header("<season><title>Season</title></season>");
    assert_eq!(
        resolve_nfo_scope(NfoLayer::Season, path, &[UnitKey::new(2, 4)], &coords),
        Some(EntityScope::Season(2))
    );
    assert_eq!(
        resolve_nfo_scope(NfoLayer::Season, path, &[UnitKey::new(1, 3)], &coords),
        None
    );
    let episode_root =
        header("<episodedetails><season>2</season><episode>4</episode></episodedetails>");
    assert_eq!(
        resolve_nfo_scope(NfoLayer::Season, path, &[UnitKey::new(2, 4)], &episode_root),
        None
    );
}

#[test]
fn work_sidecar_refuses_episode_roots() {
    let path = Path::new("/lib/Show/Season 2/Show.S02E04.mkv");
    let tvshow = header("<tvshow><title>Show</title></tvshow>");
    assert_eq!(
        resolve_nfo_scope(NfoLayer::Work, path, &[UnitKey::new(2, 4)], &tvshow),
        Some(EntityScope::Work)
    );
    let episode_root =
        header("<episodedetails><season>2</season><episode>4</episode></episodedetails>");
    assert_eq!(
        resolve_nfo_scope(NfoLayer::Work, path, &[UnitKey::new(2, 4)], &episode_root),
        None
    );
}
