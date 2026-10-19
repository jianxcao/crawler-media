use std::path::Path;

use domain::{Confidence, FileVersion, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use rusqlite::params;

use super::FileArtwork;
use crate::{Store, StoreError};

fn movie_media(store: &Store, title: &str) -> MediaId {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.to_string(),
        year: None,
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

fn ledger_row(media_id: MediaId, path: &Path) -> LedgerRow {
    LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: path.to_string_lossy().to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    }
}

fn card_artwork(
    store: &Store,
    library_id: &str,
    media_id: MediaId,
) -> (Option<String>, Option<String>, bool) {
    store
        .library
        .query_row(
            "SELECT poster_ref, backdrop_ref, has_still FROM library_items
             WHERE library_id = ?1 AND media_id = ?2",
            params![library_id, media_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0)),
        )
        .unwrap()
}

#[test]
fn scanned_artwork_reaches_the_card() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("data")).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[tmp.path().join("movies").to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id.as_deref() == Some(library.id.as_str()))
        .unwrap();
    let media = movie_media(&store, "Scanned");

    let dir = Path::new(&root.path).join("Scanned");
    std::fs::create_dir_all(&dir).unwrap();
    let video = dir.join("Scanned.mkv");
    std::fs::write(&video, b"video").unwrap();
    std::fs::write(dir.join("poster.jpg"), b"poster").unwrap();
    std::fs::write(dir.join("fanart.jpg"), b"fanart").unwrap();
    std::fs::write(dir.join("Scanned-thumb.jpg"), b"still").unwrap();

    let row = ledger_row(media, &video);
    store.insert_scanned_ledger(&row, 0).unwrap();

    let file_id = row.id.to_string();
    let (poster, backdrop, has_still) = card_artwork(&store, &library.id, media);
    assert_eq!(poster.as_deref(), Some(file_id.as_str()));
    assert_eq!(backdrop.as_deref(), Some(file_id.as_str()));
    assert!(has_still);
}

#[test]
fn recorded_artwork_updates_the_card_and_stale_versions_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("data")).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[tmp.path().join("movies").to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id.as_deref() == Some(library.id.as_str()))
        .unwrap();
    let media = movie_media(&store, "Recorded");

    let dir = Path::new(&root.path).join("Recorded");
    std::fs::create_dir_all(&dir).unwrap();
    let video = dir.join("Recorded.mkv");
    std::fs::write(&video, b"video").unwrap();
    let row = ledger_row(media, &video);
    store.insert_ledger(&row).unwrap();
    let (poster, backdrop, has_still) = card_artwork(&store, &library.id, media);
    assert_eq!((poster, backdrop, has_still), (None, None, false));

    let poster_path = dir.join("poster.jpg");
    let backdrop_path = dir.join("fanart.jpg");
    let still_path = dir.join("Recorded-thumb.jpg");
    for path in [&poster_path, &backdrop_path, &still_path] {
        std::fs::write(path, b"image").unwrap();
    }
    let (file_id, version) = store
        .file_version_for_path(&row.path)
        .unwrap()
        .expect("published file");
    assert_eq!(file_id, row.id);

    let changed = store
        .record_file_artwork(
            file_id,
            &version,
            &FileArtwork {
                poster: Some(&poster_path),
                backdrop: Some(&backdrop_path),
                still: Some(&still_path),
            },
        )
        .unwrap();
    assert!(changed);

    let file_id_str = file_id.to_string();
    let (poster, backdrop, has_still) = card_artwork(&store, &library.id, media);
    assert_eq!(poster.as_deref(), Some(file_id_str.as_str()));
    assert_eq!(backdrop.as_deref(), Some(file_id_str.as_str()));
    assert!(has_still);

    let stale = FileVersion {
        revision: version.revision + 1,
        source_version: version.source_version.clone(),
    };
    let error = store
        .record_file_artwork(
            file_id,
            &stale,
            &FileArtwork {
                poster: Some(&poster_path),
                backdrop: None,
                still: None,
            },
        )
        .unwrap_err();
    assert!(
        matches!(&error, StoreError::Protected(message) if message.contains("StaleVersion")),
        "unexpected error: {error}"
    );
}
