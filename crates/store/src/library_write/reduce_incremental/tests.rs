use domain::{
    Confidence, FilePresence, FilePublication as DomainPublication, FileVersion, LedgerId,
    LibraryFile, LibraryId, Media, MediaId, MediaKind, QualitySource, UnitKey,
};
use rusqlite::params;

use crate::Store;
use crate::library_write::{FilePublication as StorePublication, WriteScope};

/// Fixture scan row: the file identity plus the unit the scan observed, and no
/// observation facts. The read shape exposes the id and path directly, so the
/// fixture no longer wraps a legacy ledger row.
fn scanned_row(media_id: MediaId, path: &str, season: u32, episode: u32) -> crate::ScannedLedger {
    crate::ScannedLedger {
        file_id: LedgerId::new(),
        media_id,
        path: path.to_string(),
        units: vec![UnitKey::new(season as i32, episode as i32)],
        resolution: Some("1080p".to_string()),
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        source_version: "fixture-v1".into(),
        source_digest: None,
        size_bytes: None,
        created_ns: None,
        modified_ns: None,
        artwork: Default::default(),
        observations: Vec::new(),
    }
}

struct Fixture {
    store: Store,
    _tmp: tempfile::TempDir,
    library: LibraryId,
    media: MediaId,
    root: String,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        let library = store
            .create_library(MediaKind::Tv, "Shows", &["/tv"], "everyone", true, &[])
            .unwrap();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Show".to_string(),
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
        let root = store
            .list_library_roots()
            .unwrap()
            .into_iter()
            .find(|root| root.library_id.as_str() == library.id.as_str())
            .unwrap()
            .id;
        Fixture {
            _tmp: tmp,
            store,
            library: library.id.parse().unwrap(),
            media: media.id,
            root,
        }
    }

    fn scope(&self) -> WriteScope {
        WriteScope {
            library_id: self.library,
            config_revision: 0,
        }
    }

    fn file(
        &self,
        file_id: LedgerId,
        path: &str,
        presence: FilePresence,
        publication: DomainPublication,
        source_version: &str,
        size: Option<u64>,
        resolution: Option<&str>,
        hdr: Option<&str>,
        created_ns: Option<i64>,
        poster: bool,
        backdrop: bool,
        still: bool,
    ) -> LibraryFile {
        LibraryFile {
            file_id,
            library_id: self.library,
            root_id: self.root.clone(),
            media_id: self.media,
            kind: MediaKind::Tv,
            path: path.into(),
            version: FileVersion {
                revision: 1,
                source_version: source_version.to_string(),
            },
            presence,
            publication,
            size_bytes: size,
            stat_known: size.is_some(),
            created_ns,
            modified_ns: None,
            source_digest: None,
            source_path: None,
            transfer_mode: None,
            resolution: resolution.map(str::to_string),
            codec: None,
            hdr: hdr.map(str::to_string),
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
            release_quality: None,
            duration_ms: None,
            tracks_version: 0,
            poster_path: poster.then(|| format!("{path}.poster.jpg").into()),
            backdrop_path: backdrop.then(|| format!("{path}.backdrop.jpg").into()),
            still_path: still.then(|| format!("{path}.still.jpg").into()),
        }
    }

    fn publish(&self, file: &LibraryFile, units: &[UnitKey]) {
        self.store
            .publish_files(
                &self.scope(),
                &[StorePublication {
                    file,
                    units,
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false,
                }],
                0,
            )
            .unwrap();
    }

    /// The incremental path must leave exactly what a full rebuild would leave.
    fn assert_matches_full(&self) {
        let lib = self.library.to_string();
        let media = self.media.to_string();
        let incremental = snapshot(&self.store, &lib, &media);
        self.store.refresh_media_summaries(self.media, 0).unwrap();
        let full = snapshot(&self.store, &lib, &media);
        assert_eq!(
            incremental, full,
            "incremental summary drifted from full recompute"
        );
    }
}

/// Item row, unit rows and terms of one card, in a stable order. `item_revision` and
/// `unit_revision` are counters that both paths bump, so they are not compared.
fn snapshot(store: &Store, lib: &str, media: &str) -> Vec<String> {
    let mut out = Vec::new();
    {
        let mut stmt = store
            .library
            .prepare(
                "SELECT kind, title, title_sort, year, identity_state, aliases_json,
                        metadata_revision, release_date_sort, rating_sort, runtime_sort,
                        language, added_at_ns, added_at_sort, size_bytes, size_sort,
                        resolution_rank, hdr, scraped, has_nfo, missing, stable_shuffle_key,
                        file_count, available_file_count, missing_file_count,
                        unknown_file_count, distinct_episode_count, distinct_season_count,
                        preferred_file_id, has_still, poster_ref, backdrop_ref
                 FROM library_items WHERE library_id = ?1 AND media_id = ?2",
            )
            .unwrap();
        let mut rows = stmt.query(params![lib, media]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for index in 0..31 {
                out.push(format!("item[{index}]={:?}", row.get_ref(index).unwrap()));
            }
        }
    }
    {
        let mut stmt = store
            .library
            .prepare(
                "SELECT season, episode, file_count, available_file_count,
                        preferred_file_id, quality_json
                 FROM library_units WHERE library_id = ?1 AND media_id = ?2
                 ORDER BY season, episode",
            )
            .unwrap();
        let mut rows = stmt.query(params![lib, media]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for index in 0..6 {
                out.push(format!("unit[{index}]={:?}", row.get_ref(index).unwrap()));
            }
        }
    }
    {
        let mut stmt = store
            .library
            .prepare(
                "SELECT term_kind, term_value FROM library_item_terms
                 WHERE library_id = ?1 AND media_id = ?2 ORDER BY term_kind, term_value",
            )
            .unwrap();
        let mut rows = stmt.query(params![lib, media]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            out.push(format!(
                "term={:?}/{:?}",
                row.get_ref(0).unwrap(),
                row.get_ref(1).unwrap()
            ));
        }
    }
    out
}

/// Insert two scanned files and update their presence, size, artwork and quality.
fn insert_and_update(fixture: &Fixture) -> (LedgerId, LedgerId) {
    let a_row = scanned_row(fixture.media, "/tv/show/S01E01.mkv", 1, 1);
    let b_row = scanned_row(fixture.media, "/tv/show/S01E02.mkv", 1, 2);
    fixture
        .store
        .insert_scanned_files(&[a_row.clone()], 1)
        .unwrap();
    fixture
        .store
        .insert_scanned_files(&[b_row.clone()], 2)
        .unwrap();

    let a_missing = fixture.file(
        a_row.file_id,
        "/tv/show/S01E01.mkv",
        FilePresence::Missing,
        DomainPublication::Published,
        "v1",
        Some(1_000),
        Some("1080p"),
        None,
        None,
        true,
        false,
        true,
    );
    fixture.publish(&a_missing, &[UnitKey::new(1, 1)]);

    let b_present = fixture.file(
        b_row.file_id,
        "/tv/show/S01E02.mkv",
        FilePresence::Present,
        DomainPublication::Published,
        "v1",
        Some(2_000),
        Some("2160p"),
        Some("HDR10"),
        None,
        false,
        true,
        false,
    );
    fixture.publish(&b_present, &[UnitKey::new(1, 2)]);
    (a_row.file_id, b_row.file_id)
}

/// Add a file, withdraw it, lose every present file, then restore the newest revision.
fn withdraw_and_restore(fixture: &Fixture, a_id: LedgerId, b_id: LedgerId) {
    let c_id = LedgerId::new();
    let c_published = fixture.file(
        c_id,
        "/tv/show/S02E01.mkv",
        FilePresence::Present,
        DomainPublication::Published,
        "v1",
        Some(3_000),
        Some("720p"),
        None,
        Some(50),
        true,
        false,
        false,
    );
    fixture.publish(&c_published, &[UnitKey::new(2, 1)]);

    let c_pending = fixture.file(
        c_id,
        "/tv/show/S02E01.mkv",
        FilePresence::Present,
        DomainPublication::Pending,
        "v1",
        Some(3_000),
        Some("720p"),
        None,
        Some(50),
        true,
        false,
        false,
    );
    fixture.publish(&c_pending, &[UnitKey::new(2, 1)]);

    let b_missing = fixture.file(
        b_id,
        "/tv/show/S01E02.mkv",
        FilePresence::Missing,
        DomainPublication::Published,
        "v1",
        Some(2_000),
        Some("2160p"),
        Some("HDR10"),
        None,
        false,
        true,
        false,
    );
    fixture.publish(&b_missing, &[UnitKey::new(1, 2)]);

    let a_restored = fixture.file(
        a_id,
        "/tv/show/S01E01.mkv",
        FilePresence::Present,
        DomainPublication::Published,
        "v2",
        Some(1_000),
        Some("1080p"),
        None,
        None,
        true,
        false,
        true,
    );
    fixture.publish(&a_restored, &[UnitKey::new(1, 1)]);
}

#[test]
fn incremental_reduction_matches_full_recompute() {
    let fixture = Fixture::new();

    let (a_id, b_id) = insert_and_update(&fixture);
    fixture.assert_matches_full();

    withdraw_and_restore(&fixture, a_id, b_id);
    fixture.assert_matches_full();

    // A two-file scan window reduces the media once through the batch entry point.
    let d_row = scanned_row(fixture.media, "/tv/show/S03E01.mkv", 3, 1);
    let e_row = scanned_row(fixture.media, "/tv/show/S03E02.mkv", 3, 2);
    fixture
        .store
        .insert_scanned_files(&[d_row.clone(), e_row.clone()], 9)
        .unwrap();
    fixture.assert_matches_full();
}
