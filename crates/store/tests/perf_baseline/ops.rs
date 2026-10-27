//! The measured operations. One function per hot path, each returning a [`Report`].
//!
//! The set is chosen from the plan's Task 17 list plus the production read path that the
//! HTTP layer actually runs: browse (page 1, deep offset, deep cursor, and the same page
//! through a `LibraryReadPool` lease), item detail, every facet dimension, playback source
//! selection, scan publication (fresh insert and idempotent republish) and an integrity
//! status lookup. A deliberately expensive read is measured last as a control: it proves
//! the full-scan counter is live and that the gate rejects it.

use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use domain::{TypedItemId, UnitKey};
use store::library_query::{
    BrowseFilter, BrowseOrder, BrowsePage, FacetRequest, FacetTier, LibraryQuery,
};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::stats::StatsScope;
use store::playback::{BoundPlaybackEvent, PlaybackEventType, PlaybackSource};
use store::{LibraryReadRequest, QueryBudget, ReadCredential};

use super::fixture::Fixture;
use super::measure::{measure, Report, Work};

/// Browse page size, matching what the gallery asks for.
const PAGE: usize = 60;
/// Publication batch cap enforced by `publish_files`.
const PUBLISH_BATCH: usize = 100;

pub const BROWSE_PAGE_1: &str = "browse page 1 (offset 0)";
pub const BROWSE_DEEP_OFFSET: &str = "browse deep offset";
pub const BROWSE_DEEP_CURSOR: &str = "browse deep cursor";
pub const BROWSE_READ_LEASE: &str = "browse via read lease (production path)";
pub const ITEM_DETAIL: &str = "item detail (movie)";
pub const ITEM_DETAIL_TV: &str = "item detail (tv work)";
pub const PLAYBACK_SOURCES: &str = "playback source selection";
pub const PLAYBACK_ROSTER: &str = "playback authorization roster";
pub const PLAYBACK_UNSCOPED: &str = "playback source selection (no user scope)";
pub const PUBLISH_INSERT: &str = "scan publication (fresh insert)";
pub const PUBLISH_REPUBLISH: &str = "scan publication (idempotent republish)";
pub const INTEGRITY_STATUS: &str = "integrity status lookup";
pub const PLAYBACK_PROGRESS_WRITE: &str = "playback progress write";
pub const CONTROL_FULL_SCAN: &str = "control: full ledger coverage scan";

/// The ten facet dimensions the browse UI renders. Each is measured as *the selected*
/// dimension, which is the case that runs the "exclude own dimension" counting rule.
pub const FACET_DIMENSIONS: [&str; 10] = [
    "genres",
    "countries",
    "decades",
    "watch",
    "ratings",
    "runtimes",
    "languages",
    "resolutions",
    "hdr",
    "stock",
];

pub fn facet_label(dimension: &str) -> String {
    format!("facets: {dimension}")
}

/// Everything the gate measures, in a stable order.
pub fn all(fx: &Fixture) -> Vec<Report> {
    let mut reports = reads(fx);
    reports.extend(publication(fx));
    reports.push(playback_progress_write(fx));
    reports.push(control_full_scan(fx));
    reports
}

/// The read half of the gate. Every one of these is idempotent, so the DDL write-cost probe
/// can run them twice on one fixture (with and without an index) and compare the counters.
pub fn reads(fx: &Fixture) -> Vec<Report> {
    reads_impl(fx, true)
}

/// The same reads minus the read-lease path, which validates the whole layout on every
/// request: dropping an index makes it refuse the database (`LayoutMismatch`) instead of
/// degrading, which is a deliberate contract and is why the write-cost probe skips it.
pub fn reads_without_layout_guard(fx: &Fixture) -> Vec<Report> {
    reads_impl(fx, false)
}

fn reads_impl(fx: &Fixture, lease: bool) -> Vec<Report> {
    let mut reports = vec![
        browse_page_one(fx),
        browse_deep_offset(fx),
        browse_deep_cursor(fx),
    ];
    if lease {
        reports.push(browse_read_lease(fx));
    }
    reports.push(item_detail(fx));
    reports.push(item_detail_tv(fx));
    reports.extend(facets(fx));
    reports.push(playback_roster(fx));
    reports.push(playback_sources_unscoped(fx));
    reports.push(playback_sources(fx));
    reports.push(integrity_status(fx));
    reports
}

/// The production browse query: title order, a user (so watch facts join), first page with
/// the total, `PAGE` cards.
fn library_query(fx: &Fixture, page: BrowsePage, include_total: bool) -> LibraryQuery {
    LibraryQuery {
        library_id: fx.movies.library_id,
        user_id: Some(fx.user),
        filter: BrowseFilter::default(),
        order: BrowseOrder::Title,
        descending: false,
        page,
        include_total,
    }
}

/// The standard page-1 query, exposed so the seam-liveness control (`gate.rs`) can run one
/// keyed read without going through a measured operation.
pub fn page_query(fx: &Fixture) -> LibraryQuery {
    library_query(
        fx,
        BrowsePage::Offset {
            start: 0,
            limit: PAGE,
        },
        true,
    )
}

fn browse_page_one(fx: &Fixture) -> Report {
    browse_report(fx, BROWSE_PAGE_1, PAGE)
}

/// Browse page 1 with an explicit page size; the negative controls reuse it on a small
/// fixture.
pub fn browse_report(fx: &Fixture, label: &str, limit: usize) -> Report {
    let query = library_query(fx, BrowsePage::Offset { start: 0, limit }, true);
    let mut cards = 0;
    let report = measure(&fx.store, label, 3, 20, || {
        let (page, stats) = fx.store.library_item_page_with_stats(&query).unwrap();
        cards = page.items.len();
        Work::from_stats(&stats)
    });
    assert_eq!(cards, limit, "{label}: wrong card count");
    report
}

fn browse_deep_offset(fx: &Fixture) -> Report {
    let start = fx.scale.deep_offset();
    let query = library_query(fx, BrowsePage::Offset { start, limit: PAGE }, true);
    let report = measure(&fx.store, BROWSE_DEEP_OFFSET, 3, 20, || {
        let (page, stats) = fx.store.library_item_page_with_stats(&query).unwrap();
        assert_eq!(page.items.len(), PAGE);
        Work::from_stats(&stats)
    });
    // The plan's D5 finding: the API only ever sends an offset page, so the deep page pays
    // for the total on every request. Both numbers below are the evidence.
    report
}

fn browse_deep_cursor(fx: &Fixture) -> Report {
    // `BrowsePage::Cursor` is not reachable from the HTTP layer yet (plan Task 14); this
    // measures the store query the API will call once cursor paging lands. The cursor is
    // reached by draining cursor pages the way the client will, so the measured page is
    // the same depth as the offset one.
    let after = deep_cursor_anchor(fx);
    let query = library_query(fx, BrowsePage::Cursor { after: Some(after), limit: PAGE }, false);
    measure(&fx.store, BROWSE_DEEP_CURSOR, 3, 20, || {
        let (page, stats) = fx.store.library_item_page_with_stats(&query).unwrap();
        assert_eq!(page.items.len(), PAGE);
        Work::from_stats(&stats)
    })
}

/// Drains cursor pages until the deep offset is passed; returns the last cursor.
fn deep_cursor_anchor(fx: &Fixture) -> String {
    let target = fx.scale.deep_offset();
    let mut after: Option<String> = None;
    let mut taken = 0;
    loop {
        let query = library_query(
            fx,
            BrowsePage::Cursor {
                after: after.clone(),
                limit: PAGE,
            },
            false,
        );
        let page = fx.store.library_item_page(&query).unwrap();
        taken += page.items.len();
        match page.next_cursor {
            Some(cursor) if taken < target => after = Some(cursor),
            Some(cursor) => return cursor,
            None => panic!("cursor drain ended before the deep offset"),
        }
    }
}

/// The same page through the read pool, which is what a member request runs: checkout,
/// layout validation, `BEGIN`, roster verification, selection, hydration, `ROLLBACK`.
fn browse_read_lease(fx: &Fixture) -> Report {
    let pool = fx
        .store
        .open_library_read_pool(NonZeroUsize::new(2).unwrap())
        .unwrap();
    let credential = ReadCredential::new(fx.user, fx.token.clone());
    let request = LibraryReadRequest::Browse(library_query(
        fx,
        BrowsePage::Offset { start: 0, limit: PAGE },
        true,
    ));
    let context = fx.store.prepare_library_read(&credential, &request).unwrap();
    let report = measure(&fx.store, BROWSE_READ_LEASE, 1, 10, || {
        let budget = QueryBudget {
            deadline: Instant::now() + Duration::from_secs(30),
            cancelled: Default::default(),
        };
        let mut lease = pool.try_checkout().expect("read lease");
        let (result, stats) = lease.execute_observed(&context, &budget);
        assert!(result.is_ok(), "leased browse failed: {:?}", result.err());
        Work::from_stats(&stats)
    });
    pool.close();
    report
}

fn item_detail(fx: &Fixture) -> Report {
    let media = fx.movie_media(7);
    let locale = fx.locale.clone();
    let library = fx.movies.library_id;
    measure(&fx.store, ITEM_DETAIL, 3, 20, || {
        let (detail, stats) = fx.store.with_stats(StatsScope::Library, || {
            fx.store
                .library_item_detail(library, media, &locale)
                .unwrap()
                .expect("seeded media has a detail row")
        });
        assert_eq!(detail.files.len(), 2, "the seeded movie has two versions");
        Work::from_stats(&stats)
    })
}

/// The TV detail path: season/episode unit coverage plus the file page. Separate from the
/// movie detail because a work with units reads different tables.
fn item_detail_tv(fx: &Fixture) -> Report {
    let media = fx.show_media(3);
    let locale = fx.locale.clone();
    let library = fx.shows.library_id;
    let episodes = fx.scale.episodes_per_show;
    measure(&fx.store, ITEM_DETAIL_TV, 3, 20, || {
        let (detail, stats) = fx.store.with_stats(StatsScope::Library, || {
            fx.store
                .library_item_detail(library, media, &locale)
                .unwrap()
                .expect("seeded show has a detail row")
        });
        assert_eq!(detail.files.len(), episodes, "every episode file is listed");
        assert_eq!(detail.seasons, vec![1]);
        Work::from_stats(&stats)
    })
}

/// The filter each facet dimension contributes when it is the selected dimension.
pub fn facet_filter(dimension: &str) -> Option<BrowseFilter> {
    let mut filter = BrowseFilter::default();
    match dimension {
        "genres" => filter.genres = vec!["Drama".into()],
        "countries" => filter.countries = vec!["US".into()],
        "decades" => filter.decades = vec!["2010s".into()],
        "watch" => filter.watch = Some("unwatched".into()),
        "ratings" => filter.min_rating = Some(7.0),
        "runtimes" => filter.runtimes = vec!["90to120".into()],
        "languages" => filter.languages = vec!["en".into()],
        "resolutions" => filter.resolutions = vec!["1080p".into()],
        "hdr" => filter.hdr = Some(true),
        "stock" => filter.stock = vec!["unscraped".into()],
        _ => return None,
    }
    Some(filter)
}

fn facets(fx: &Fixture) -> Vec<Report> {
    FACET_DIMENSIONS
        .iter()
        .map(|dimension| facet_report(fx, dimension))
        .collect()
}

/// The same page query run twice per request: the N+1 shape the statement budget rejects.
pub fn browse_report_twice(fx: &Fixture, label: &str, limit: usize) -> Report {
    let query = library_query(fx, BrowsePage::Offset { start: 0, limit }, true);
    measure(&fx.store, label, 3, 5, || {
        let (first, mut stats) = fx.store.library_item_page_with_stats(&query).unwrap();
        let (second, again) = fx.store.library_item_page_with_stats(&query).unwrap();
        assert_eq!(first.items.len(), limit);
        assert_eq!(second.items.len(), limit);
        stats.statements += again.statements;
        stats.fullscan_steps += again.fullscan_steps;
        stats.vm_steps += again.vm_steps;
        Work::from_stats(&stats)
    })
}

/// One facet dimension's request; public because the negative controls measure a single
/// dimension.
pub fn facet_report(fx: &Fixture, dimension: &str) -> Report {
    let request = FacetRequest {
        library_id: fx.movies.library_id,
        user_id: Some(fx.user),
        filter: facet_filter(dimension).expect("known dimension"),
        tier: FacetTier::All,
    };
    let label = facet_label(dimension);
    measure(&fx.store, &label, 3, 10, || {
        let (facets, stats) = fx.store.library_facets_with_stats(&request).unwrap();
        assert_eq!(facets.groups.len(), FACET_DIMENSIONS.len());
        Work::from_stats(&stats)
    })
}

/// The authorization half of playback source selection: one roster read per request.
fn playback_roster(fx: &Fixture) -> Report {
    let user = fx.user;
    measure(&fx.store, PLAYBACK_ROSTER, 3, 20, || {
        let (allowed, stats) = fx
            .store
            .with_stats(StatsScope::Both, || fx.store.authorized_library_ids(user).unwrap());
        assert!(allowed.contains(&fx.movies.library_id.to_string()));
        Work::from_stats(&stats)
    })
}

/// Source selection of the public artwork route: same query, no user scope.
fn playback_sources_unscoped(fx: &Fixture) -> Report {
    let item = TypedItemId::Work(fx.movie_media(7));
    measure(&fx.store, PLAYBACK_UNSCOPED, 3, 20, || {
        let (sources, stats) = fx
            .store
            .with_stats(StatsScope::Library, || fx.store.published_entries_for_item(&item).unwrap());
        assert_eq!(sources.len(), 2);
        Work::from_stats(&stats)
    })
}

fn playback_sources(fx: &Fixture) -> Report {
    let item = TypedItemId::Work(fx.movie_media(7));
    let user = fx.user;
    measure(&fx.store, PLAYBACK_SOURCES, 3, 20, || {
        let (sources, stats) = fx
            .store
            .with_stats(StatsScope::Both, || fx.store.playable_entries(user, &item).unwrap());
        assert_eq!(sources.len(), 2, "both published versions must be selectable");
        assert_eq!(sources[0].unit, UnitKey::WHOLE);
        Work::from_stats(&stats)
    })
}

/// Publishes the reserved Media, one batch at a time. The deterministic write facts (how
/// many rows were inserted, how many were already current) are asserted by `gate`.
pub fn publication(fx: &Fixture) -> Vec<Report> {
    let slots = fx.scale.publish_slots;
    let insert = measure(&fx.store, PUBLISH_INSERT, 1, 1, || publish_slots(fx, 0, slots));
    let republish = measure(&fx.store, PUBLISH_REPUBLISH, 0, 3, || publish_slots(fx, 0, slots));
    vec![insert, republish]
}

/// Publishes `count` reserved slots starting at `start`. Public so the DDL write-cost probe
/// publishes the identical unit of work.
pub fn publish_slot_range(fx: &Fixture, start: usize, count: usize) {
    let _ = publish_slots(fx, start, count);
}

fn publish_slots(fx: &Fixture, start: usize, count: usize) -> Work {
    let config_revision = fx
        .store
        .library_config_revision(&fx.movies.library_id.to_string())
        .unwrap();
    let scope = WriteScope {
        library_id: fx.movies.library_id,
        config_revision,
    };
    let mut inserted = 0;
    let mut updated = 0;
    let mut unchanged = 0;
    // The batch is the unit the scanner really publishes, so the counters are for the whole
    // batch (every `PUBLISH_BATCH`-sized chunk), not for one chunk.
    let (_, stats) = fx.store.with_stats(StatsScope::Library, || {
        for chunk_start in (start..start + count).step_by(PUBLISH_BATCH) {
            let end = (chunk_start + PUBLISH_BATCH).min(start + count);
            let rows: Vec<_> = (chunk_start..end)
                .map(|index| (fx.slot_file(index), vec![UnitKey::WHOLE]))
                .collect();
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
            let report = fx.store.publish_files(&scope, &publications, 1_000).unwrap();
            inserted += report.inserted;
            updated += report.updated;
            unchanged += report.unchanged;
        }
    });
    let first_pass = inserted == count && updated == 0 && unchanged == 0;
    let repeat_pass = inserted == 0 && updated == 0 && unchanged == count;
    assert!(
        first_pass || repeat_pass,
        "publication of {count} files reported an unexpected shape: \
         inserted {inserted}, updated {updated}, unchanged {unchanged}"
    );
    Work::from_stats(&stats)
}

/// The playback heartbeat: one progress event per client tick. This is the write path whose
/// cost the v3 DDL's `playback_*` indexes show up in, and it runs once per active player
/// every few seconds in production.
pub fn playback_progress_write(fx: &Fixture) -> Report {
    let mut session = ProgressSession::open(fx);
    measure(&fx.store, PLAYBACK_PROGRESS_WRITE, 1, 50, || {
        let (_, stats) = fx.store.with_stats(StatsScope::Library, || {
            session.commit_next(&fx.store);
        });
        Work::from_stats(&stats)
    })
}

/// An issued play session plus the source it was issued for. Public because the DDL
/// write-cost probe reuses it.
pub struct ProgressSession {
    user: domain::UserId,
    device: &'static str,
    item: TypedItemId,
    source: PlaybackSource,
    session_id: String,
    generation: i64,
    sequence: i64,
    now: i64,
}

impl ProgressSession {
    pub fn open(fx: &Fixture) -> Self {
        let media = fx.movie_media(7);
        let item = TypedItemId::Work(media);
        let source = fx
            .store
            .playable_entries(fx.user, &item)
            .unwrap()
            .into_iter()
            .next()
            .expect("the seeded movie has a published file");
        let device = "perf-client";
        let now = 1_700_000_000;
        let session_id = fx
            .store
            .issue_play_session(
                fx.user,
                device,
                source.file.file_id,
                media,
                source.unit,
                &source.file.version,
                now,
            )
            .unwrap();
        let generation = fx
            .store
            .play_session_binding(fx.user, device, &session_id)
            .unwrap()
            .expect("issued binding")
            .generation;
        Self {
            user: fx.user,
            device,
            item,
            source,
            session_id,
            generation,
            sequence: 0,
            now,
        }
    }

    /// Commits the next progress event of this session.
    pub fn commit_next(&mut self, store: &store::Store) {
        self.sequence += 1;
        let sequence = self.sequence;
        let event = BoundPlaybackEvent {
            user_id: self.user,
            device_id: self.device.into(),
            session_id: self.session_id.clone(),
            generation: self.generation,
            event_sequence: Some(sequence),
            item: self.item.clone(),
            source_id: domain::PhysicalSourceId {
                file_id: self.source.file.file_id,
                revision: self.source.file.version.revision,
            },
            source_version: self.source.file.version.source_version.clone(),
            position_ms: sequence * 1_000,
            duration_ms: self.source.file.duration_ms,
            audio_track: None,
            subtitle_track: None,
            paused: false,
            rate_bps: 0,
            client: Some("perf".into()),
            device_name: None,
            client_version: None,
            play_method: None,
            event_type: PlaybackEventType::Progress,
        };
        store
            .commit_playback_event(&event, self.now + sequence)
            .unwrap();
    }
}

fn integrity_status(fx: &Fixture) -> Report {
    let operation_id = fx.integrity_operation_id.clone();
    measure(&fx.store, INTEGRITY_STATUS, 3, 20, || {
        let ((status, view, report), stats) = fx.store.with_stats(StatsScope::Library, || {
            let status = fx.store.get_library_operation(&operation_id).unwrap();
            let view = fx.store.library_operation_view(&operation_id).unwrap();
            let report = fx.store.library_integrity_report(&operation_id).unwrap();
            (status, view, report)
        });
        assert!(status.is_some(), "the fixture seeds one operation row");
        assert!(view.is_some());
        assert!(!report.has_issues());
        Work::from_stats(&stats)
    })
}

/// The known-expensive read: `list_file_coverage` scans every file row. It is here as a
/// control — its full-scan counter is non-zero, which is how the gate proves that its
/// zero-full-scan limits are meaningful and not just an unset field.
fn control_full_scan(fx: &Fixture) -> Report {
    // The publication measurement has already published the reserved slots.
    let expected = fx.scale.files() + fx.scale.publish_slots;
    let mut rows = 0;
    let report = measure(&fx.store, CONTROL_FULL_SCAN, 0, 1, || {
        let (coverage, stats) = fx
            .store
            .with_stats(StatsScope::Library, || fx.store.list_file_coverage().unwrap());
        rows = coverage.len();
        Work::from_stats(&stats)
    });
    assert_eq!(rows, expected, "coverage listed the wrong file count");
    report
}

/// Measures `Store::list_file_coverage` — the read that scans every file row. This is the
/// control (its ledger-scan counter is non-zero) and the negative control's stand-in for
/// the D1 regression (`delivery.rs` used to call it inside the Store lock).
pub fn ledger_scan_report(fx: &Fixture, label: &str) -> Report {
    let mut rows = 0;
    let report = measure(&fx.store, label, 0, 1, || {
        let (coverage, stats) = fx
            .store
            .with_stats(StatsScope::Library, || fx.store.list_file_coverage().unwrap());
        rows = coverage.len();
        Work::from_stats(&stats)
    });
    assert!(rows > 0, "coverage listed no files");
    report
}

/// Detail + a full ledger scan: what the D1 regression would look like if it came back.
pub fn detail_with_ledger_scan_report(fx: &Fixture, label: &str) -> Report {
    let media = fx.movie_media(1);
    let locale = fx.locale.clone();
    let library = fx.movies.library_id;
    measure(&fx.store, label, 3, 5, || {
        let (_, stats) = fx.store.with_stats(StatsScope::Library, || {
            fx.store
                .library_item_detail(library, media, &locale)
                .unwrap()
                .expect("seeded media has a detail row");
            let _ = fx.store.list_file_coverage().unwrap();
        });
        Work::from_stats(&stats)
    })
}
