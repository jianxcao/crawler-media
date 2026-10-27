//! The deterministic seeded fixture behind the performance gate.
//!
//! Every id is a UUID v5 of a fixed label and every fact is a pure function of the row
//! index, so the same [`Scale`] always produces byte-identical `library.db` content and the
//! measured counters are reproducible. Rows are written through the store API
//! (`insert_media`, `publish_files`, `set_metadata_overrides`, `mutate_playback`): no
//! sleeps, no network, no Chromium, no files on disk beyond the databases.
//!
//! Shape (see [`Scale::REALISTIC`]): 3 Libraries / 20k Media / 60k files, 5k TV works with
//! 9 episodes each, 10 facet dimensions populated (genres, countries, decade, watch,
//! ratings, runtimes, languages, resolutions, hdr, stock).

use domain::{
    Confidence, EntityScope, FieldValue, FilePresence, FilePublication, FileVersion, LedgerId,
    LibraryFile, LibraryId, Media, MediaId, MediaKind, MetadataFields, MetadataGenre, QualitySource,
    UnitKey, User, UserId, UserRole,
};
use store::Store;
use store::library_write::{FilePublication as StorePublication, WriteScope};

const NAMESPACE: uuid::Uuid = uuid::Uuid::NAMESPACE_OID;

fn fixture_id(label: &str) -> String {
    uuid::Uuid::new_v5(&NAMESPACE, label.as_bytes()).to_string()
}

fn media_id_of(label: &str) -> MediaId {
    fixture_id(&format!("perf-media-{label}")).parse().unwrap()
}

fn file_id_of(label: &str) -> LedgerId {
    fixture_id(&format!("perf-file-{label}")).parse().unwrap()
}

/// Row counts of one seeded database. `publish_slots` are Media that exist but have no file
/// yet: the scan-publication operation publishes exactly those, so the write measurement
/// never competes with the read measurements for the same rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scale {
    pub movies: usize,
    pub movie_copies: usize,
    pub shows: usize,
    pub episodes_per_show: usize,
    pub other: usize,
    pub publish_slots: usize,
}

impl Scale {
    /// 20,000 Media / 61,000 files across 3 Libraries, 5,000 TV works with 9 episodes each.
    pub const REALISTIC: Self = Self {
        movies: 14_000,
        movie_copies: 1_000,
        shows: 5_000,
        episodes_per_show: 9,
        other: 1_000,
        publish_slots: 1_000,
    };
    /// The same shape at default-`cargo test` cost. Statement and full-scan limits are
    /// scale independent (the queries, page sizes and filters are identical), so this
    /// variant runs the same gate.
    pub const SMALL: Self = Self {
        movies: 240,
        movie_copies: 24,
        shows: 40,
        episodes_per_show: 3,
        other: 40,
        publish_slots: 200,
    };
    /// Fixture for the DDL write-cost probe (Task 10): a small Library plus one full
    /// publication batch of reserved Media slots, rebuilt once per condition.
    pub const WRITE_PROBE: Self = Self {
        movies: 1_200,
        movie_copies: 120,
        shows: 120,
        episodes_per_show: 3,
        other: 120,
        // Two interleaved publication batches per index for the six v3 indexes, plus slack.
        publish_slots: 2_400,
    };
    /// Fixture for the negative-control tests: just enough rows for the broken query to
    /// show up, small enough to build in milliseconds.
    pub const TINY: Self = Self {
        movies: 90,
        movie_copies: 10,
        shows: 8,
        episodes_per_show: 2,
        other: 8,
        publish_slots: 20,
    };

    pub fn media(&self) -> usize {
        self.movies + self.shows + self.other + self.publish_slots
    }

    pub fn files(&self) -> usize {
        self.movies + self.movie_copies + self.shows * self.episodes_per_show + self.other
    }

    pub fn episodes(&self) -> usize {
        self.shows * self.episodes_per_show
    }

    /// Offset of the "deep" browse page: 40% into the seeded movie Library.
    pub fn deep_offset(&self) -> usize {
        self.movies * 2 / 5
    }

    pub fn label(&self) -> String {
        format!(
            "{} media / {} files / {} episodes / deep offset {}",
            self.media(),
            self.files(),
            self.episodes(),
            self.deep_offset()
        )
    }
}

/// One seeded Library plus the write coordinates the fixture needs after creation.
pub struct SeededLibrary {
    pub library_id: LibraryId,
    pub root_id: String,
    pub root: String,
    pub config_revision: i64,
}

/// A seeded database: the store, the three Libraries, and the user whose watch facts feed
/// the `watch` facet dimension.
pub struct Fixture {
    /// Owns the temporary data directory: dropping it deletes the databases.
    #[allow(dead_code)]
    pub dir: tempfile::TempDir,
    pub store: Store,
    pub scale: Scale,
    pub locale: String,
    pub user: UserId,
    pub token: String,
    pub movies: SeededLibrary,
    pub shows: SeededLibrary,
    /// A real `library_operations` row, so the integrity status lookup reads a row that
    /// exists instead of the empty-table branch.
    pub integrity_operation_id: String,
}

impl Fixture {
    /// Builds the fixture, reusing the seed cache when it matches this DDL and this scale.
    ///
    /// Seeding is ~50,000 write transactions (one per Media, per NFO override, per watch row),
    /// which costs 2.5 minutes on an idle disk and much more while other agents build. The
    /// cache keeps the *deterministic* promise — the key covers the DDL on disk, the SQLite
    /// version, the scale and [`FIXTURE_VERSION`], so any of those changing reseeds — and makes
    /// re-running the gate after every task cheap. Set `PERF_FIXTURE_CACHE` to relocate it,
    /// or `PERF_FIXTURE_CACHE=off` to force a fresh seed.
    pub fn build(scale: Scale) -> Self {
        if let Some(hit) = lookup(scale) {
            return hit;
        }
        match take_turn(scale) {
            CacheSlot::Ready(hit) => hit,
            CacheSlot::Owner(guard) => {
                let seeded = seed(scale);
                store_seed(&seeded);
                drop(guard);
                seeded
            }
            CacheSlot::Unavailable => seed(scale),
        }
    }

    /// Rebuilds a fixture around an already-opened Store (the seed-cache hit path). The parts
    /// are exactly what `seed` produced, so passing them individually keeps that contract
    /// explicit instead of hiding it behind another struct.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn from_parts(
        dir: tempfile::TempDir,
        store: Store,
        scale: Scale,
        locale: String,
        user: UserId,
        token: String,
        movies: SeededLibrary,
        shows: SeededLibrary,
        integrity_operation_id: String,
    ) -> Self {
        Self {
            dir,
            store,
            scale,
            locale,
            user,
            token,
            movies,
            shows,
            integrity_operation_id,
        }
    }
}

/// Seeds the fixture from scratch through the store API. Every id and fact is a pure
/// function of the scale.
fn seed(scale: Scale) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let movies = create_library(&store, MediaKind::Movie, "Perf Movies", "/perf/movies");
    let shows = create_library(&store, MediaKind::Tv, "Perf Shows", "/perf/shows");
    let other = create_library(&store, MediaKind::Movie, "Perf Other", "/perf/other");
    seed_movies(&store, &movies, &scale);
    seed_shows(&store, &shows, &scale);
    seed_other(&store, &other, &scale);
    seed_publish_slots(&store, &scale);
    seed_metadata(&store, &movies, &scale);
    let (user, token) = seed_user(&store);
    seed_watch(&store, user, &scale);
    let locale = store::library_query::library_display_locale(&store, movies.library_id)
        .unwrap()
        .unwrap_or_else(|| "en-US".into());
    let integrity_operation_id = seed_integrity_operation(&store, &other);
    // `Store::open` seeds the two fresh-start defaults (movie, tv, both empty); the
    // fixture adds the three Libraries the measurements read.
    let libraries = store.list_libraries().unwrap();
    assert!(
        libraries.len() >= 3,
        "the fixture seeds three Libraries on top of the fresh-start defaults"
    );
    assert!(
        libraries
            .iter()
            .any(|library| library.id == shows.library_id.to_string()),
        "the TV Library is part of the roster"
    );
    Fixture::from_parts(
        dir,
        store,
        scale,
        locale,
        user,
        token,
        movies,
        shows,
        integrity_operation_id,
    )
}

impl Fixture {
    pub fn movie_media(&self, index: usize) -> MediaId {
        media_id_of(&format!("movie-{index}"))
    }

    pub fn show_media(&self, index: usize) -> MediaId {
        media_id_of(&format!("show-{index}"))
    }

    pub fn publish_slot_path(&self, index: usize) -> String {
        format!("{}/slot{:05}.mkv", self.movies.root, index)
    }

    /// The (still unpublished) file row of one reserved Media slot.
    pub fn slot_file(&self, index: usize) -> LibraryFile {
        file_row(
            &format!("slot-{index}"),
            &self.movies,
            media_id_of(&format!("slot-{index}")),
            MediaKind::Movie,
            self.publish_slot_path(index),
            self.scale.movies + index,
        )
    }
}

/// Creates one Library and reads back the root created with it.
fn create_library(store: &Store, kind: MediaKind, name: &str, root: &str) -> SeededLibrary {
    let library = store
        .create_library(kind, name, &[root], "everyone", true, &[])
        .unwrap();
    let library_id: LibraryId = library.id.parse().unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|row| row.library_id.as_str() == library.id.as_str())
        .unwrap()
        .id;
    let config_revision = store.library_config_revision(&library.id).unwrap();
    SeededLibrary {
        library_id,
        root_id,
        root: root.to_string(),
        config_revision,
    }
}

/// Inserts one Media row. Canonical facts only; metadata is applied later.
fn insert_media(store: &Store, label: &str, kind: MediaKind, title: &str, year: u16) -> MediaId {
    let id = media_id_of(label);
    store
        .insert_media(&Media {
            id,
            kind,
            title: title.to_string(),
            year: Some(year),
            original_title: None,
            tmdb_id: Some(fixture_id(&format!("tmdb-{label}"))),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
            aliases: vec![],
        })
        .unwrap();
    id
}

/// One published file row with deterministic probe facts.
#[allow(clippy::too_many_arguments)]
fn file_row(
    label: &str,
    library: &SeededLibrary,
    media_id: MediaId,
    kind: MediaKind,
    path: String,
    index: usize,
) -> LibraryFile {
    let resolution = ["1080p", "2160p", "720p"][index % 3];
    LibraryFile {
        file_id: file_id_of(label),
        library_id: library.library_id,
        root_id: library.root_id.clone(),
        media_id,
        kind,
        path: path.into(),
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(1_000_000_000 + (index as u64 % 40) * 250_000_000),
        stat_known: true,
        created_ns: Some(1_600_000_000_000_000_000 + index as i64 * 1_000_000),
        modified_ns: Some(1_600_000_000_000_000_000 + index as i64 * 1_000_000),
        source_digest: Some(fixture_id(&format!("digest-{label}"))),
        source_path: None,
        transfer_mode: None,
        resolution: Some(resolution.to_string()),
        codec: Some("h264".into()),
        hdr: Some(if index.is_multiple_of(4) { "HDR10" } else { "SDR" }.to_string()),
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(100),
        release_quality: Some(format!("{resolution} WEBDL")),
        duration_ms: Some(7_200_000),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

/// Publishes a batch (the store accepts at most 100 files per transaction).
fn publish(store: &Store, library: &SeededLibrary, rows: &mut Vec<(LibraryFile, Vec<UnitKey>)>) {
    if rows.is_empty() {
        return;
    }
    assert!(rows.len() <= 100, "the store caps one publication at 100 files");
    let publications: Vec<StorePublication<'_>> = rows
        .iter()
        .map(|(file, units)| StorePublication {
            file,
            units: units.as_slice(),
            expected_version: None,
            nfo_observation: None,
            probe_required: false,
        })
        .collect();
    store
        .publish_files(
            &WriteScope {
                library_id: library.library_id,
                config_revision: library.config_revision,
            },
            &publications,
            1_000,
        )
        .unwrap();
    rows.clear();
}

fn seed_movies(store: &Store, library: &SeededLibrary, scale: &Scale) {
    let mut batch: Vec<(LibraryFile, Vec<UnitKey>)> = Vec::with_capacity(100);
    for index in 0..scale.movies {
        let media = insert_media(
            store,
            &format!("movie-{index}"),
            MediaKind::Movie,
            &format!("Movie {index:06}"),
            1950 + (index % 70) as u16,
        );
        let path = format!("{}/movie{:06}.mkv", library.root, index);
        let file = file_row(
            &format!("movie-{index}"),
            library,
            media,
            MediaKind::Movie,
            path,
            index,
        );
        batch.push((file, vec![UnitKey::WHOLE]));
        if index < scale.movie_copies {
            let copy = file_row(
                &format!("movie-{index}-copy"),
                library,
                media,
                MediaKind::Movie,
                format!("{}/movie{:06}.2160p.mkv", library.root, index),
                index + 1,
            );
            batch.push((copy, vec![UnitKey::WHOLE]));
        }
        if batch.len() >= 99 {
            publish(store, library, &mut batch);
        }
    }
    publish(store, library, &mut batch);
}

fn seed_shows(store: &Store, library: &SeededLibrary, scale: &Scale) {
    let mut batch: Vec<(LibraryFile, Vec<UnitKey>)> = Vec::with_capacity(100);
    for show in 0..scale.shows {
        let media = insert_media(
            store,
            &format!("show-{show}"),
            MediaKind::Tv,
            &format!("Show {show:05}"),
            1990 + (show % 35) as u16,
        );
        for episode in 0..scale.episodes_per_show {
            let label = format!("show-{show}-e{episode}");
            let file = file_row(
                &label,
                library,
                media,
                MediaKind::Tv,
                format!("{}/show{show:05}/s01e{:02}.mkv", library.root, episode + 1),
                show * 10 + episode,
            );
            batch.push((file, vec![UnitKey::new(1, episode as i32 + 1)]));
            if batch.len() >= 100 {
                publish(store, library, &mut batch);
            }
        }
    }
    publish(store, library, &mut batch);
}

fn seed_other(store: &Store, library: &SeededLibrary, scale: &Scale) {
    let mut batch: Vec<(LibraryFile, Vec<UnitKey>)> = Vec::with_capacity(100);
    for index in 0..scale.other {
        let media = insert_media(
            store,
            &format!("other-{index}"),
            MediaKind::Movie,
            &format!("Other {index:05}"),
            2000 + (index % 20) as u16,
        );
        let file = file_row(
            &format!("other-{index}"),
            library,
            media,
            MediaKind::Movie,
            format!("{}/other{:05}.mkv", library.root, index),
            index,
        );
        batch.push((file, vec![UnitKey::WHOLE]));
        if batch.len() >= 100 {
            publish(store, library, &mut batch);
        }
    }
    publish(store, library, &mut batch);
}

/// Inserts the Media that the scan-publication measurement publishes.
fn seed_publish_slots(store: &Store, scale: &Scale) {
    for index in 0..scale.publish_slots {
        insert_media(
            store,
            &format!("slot-{index}"),
            MediaKind::Movie,
            &format!("Slot {index:05}"),
            2020,
        );
    }
}

/// The metadata facts behind nine of the ten facet dimensions. `hdr`/`resolution` come from
/// the published file rows instead, and `stock=unscraped` from the Media left without
/// metadata (every fifth row).
fn metadata_for(index: usize, kind: MediaKind) -> MetadataFields {
    const GENRES: [&str; 4] = ["Drama", "Comedy", "Action", "Documentary"];
    const COUNTRIES: [&str; 3] = ["US", "JP", "KR"];
    const LANGUAGES: [&str; 3] = ["en", "ja", "ko"];
    let genres = vec![
        MetadataGenre {
            source: "fixture".into(),
            source_id: None,
            name: GENRES[index % GENRES.len()].into(),
            filter_key: GENRES[index % GENRES.len()].into(),
        },
        MetadataGenre {
            source: "fixture".into(),
            source_id: None,
            name: "All".into(),
            filter_key: "All".into(),
        },
    ];
    MetadataFields {
        genres: FieldValue::Value(genres),
        countries: FieldValue::Value(vec![COUNTRIES[index % COUNTRIES.len()].into()]),
        language: FieldValue::Value(LANGUAGES[index % LANGUAGES.len()].into()),
        year: FieldValue::Value(1950 + (index % 70) as u16),
        rating: FieldValue::Value(5.0 + (index % 50) as f64 / 10.0),
        runtime_minutes: FieldValue::Value(70 + (index % 90) as u32),
        release_date: FieldValue::Value(format!("{:04}-06-15", 1950 + index % 70)),
        number_of_seasons: match kind {
            MediaKind::Tv => FieldValue::Value(1),
            _ => FieldValue::Missing,
        },
        number_of_episodes: match kind {
            MediaKind::Tv => FieldValue::Value(9),
            _ => FieldValue::Missing,
        },
        ..Default::default()
    }
}

/// Applies metadata overrides to 4 of every 5 Media, which is what a scraped Library looks
/// like; the remaining fifth keeps `stock=unscraped`.
fn seed_metadata(store: &Store, movies: &SeededLibrary, scale: &Scale) {
    let locale = store::library_query::library_display_locale(store, movies.library_id)
        .unwrap()
        .unwrap_or_else(|| "en-US".into());
    for index in 0..scale.movies {
        if index % 5 == 4 {
            continue;
        }
        store
            .set_metadata_overrides(
                media_id_of(&format!("movie-{index}")),
                EntityScope::Work,
                &locale,
                &metadata_for(index, MediaKind::Movie),
                0,
            )
            .unwrap();
    }
    for show in 0..scale.shows {
        store
            .set_metadata_overrides(
                media_id_of(&format!("show-{show}")),
                EntityScope::Work,
                &locale,
                &metadata_for(show, MediaKind::Tv),
                0,
            )
            .unwrap();
    }
}

fn seed_user(store: &Store) -> (UserId, String) {
    let user = UserId::new();
    let token = format!("perf-baseline-{user}");
    store
        .insert_user(&User {
            id: user,
            login: "perf-baseline".into(),
            enabled: true,
            role: UserRole::Member,
        })
        .unwrap();
    store.set_user_token(user, &token).unwrap();
    (user, token)
}

/// Watch facts for the `watch` facet dimension: every third movie is played, every fifth is
/// a favourite.
fn seed_watch(store: &Store, user: UserId, scale: &Scale) {
    for index in 0..scale.movies {
        let played = index % 3 == 0;
        let favorite = index % 5 == 0;
        if !played && !favorite {
            continue;
        }
        let media = media_id_of(&format!("movie-{index}"));
        store
            .mutate_playback(
                media,
                &store::playback::PlaybackMutation::marks(
                    user,
                    -1,
                    -1,
                    played.then_some(true),
                    favorite.then_some(true),
                ),
                1_700_000_000 + index as i64,
            )
            .unwrap();
    }
}

/// Creates one `library_operations` row (state `planned`) on the third Library, so the
/// integrity status lookup reads an existing row without paying for a check over the
/// measured Library.
fn seed_integrity_operation(store: &Store, library: &SeededLibrary) -> String {
    let operation_id = "perf-baseline-integrity".to_string();
    store
        .begin_library_integrity(library.library_id, false, &operation_id, 1_700_000_000)
        .unwrap();
    operation_id
}

/// Bump when the *seeding* changes (new fact, new row shape); the DDL is hashed separately.
pub const FIXTURE_VERSION: u32 = 1;

use super::fixture_cache::{CacheSlot, lookup, publish as store_seed, take_turn};
