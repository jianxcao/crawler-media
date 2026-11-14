//! L1a: Scrape poster backfill reads only the scanned directory's files, by
//! path_key range, without loading unit coverage.
use domain::{Media, MediaId, MediaKind};
use store::Store;
use store::test_support::{FixtureFile, publish_fixture_file};

#[test]
fn file_media_under_returns_only_files_inside_the_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("tv");
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "L",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = MediaId::new();
    store
        .insert_media(&Media {
            id: media,
            kind: MediaKind::Movie,
            title: "M".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let inside = root.join("anime").join("A.mkv");
    let nested = root.join("anime").join("deep").join("B.mkv");
    let sibling = root.join("anime2").join("C.mkv");
    let mut published = Vec::new();
    for path in [&inside, &nested, &sibling] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
        publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: media,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        published.push(path.clone());
    }
    let mut found = store.file_media_under(&root.join("anime")).unwrap();
    found.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(found, vec![(media, inside), (media, nested)]);
    assert!(
        store
            .file_media_under(&root.join("none"))
            .unwrap()
            .is_empty()
    );
}
