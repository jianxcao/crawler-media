//! Continuation pages preserve scope, user state, selected source and bounded work.
use domain::{LibraryId, Media, MediaId, MediaKind, UnitKey, UnitScope, UserId};
use store::playback::PlaybackMutation;
use store::stats::StatsScope;
use store::test_support::{FixtureFile, publish_fixture_file_units};
use store::{ContinueEntry, ContinueQuery, Store};

fn query(user_id: UserId, libraries: &[LibraryId], limit: usize, offset: usize) -> ContinueQuery {
    ContinueQuery {
        user_id,
        libraries: libraries.to_vec(),
        parent: domain::ProtocolParent::Root,
        media_id: None,
        season: None,
        limit,
        offset,
    }
}

fn target(entry: &ContinueEntry) -> (MediaId, LibraryId, i32, i32) {
    let (media, unit) = match &entry.reference.item {
        domain::TypedItemId::Unit { media, key } => (*media, *key),
        domain::TypedItemId::Work(media) => (*media, UnitKey::WHOLE),
        domain::TypedItemId::Season { .. } => {
            panic!("continuation selects a playable unit, never a season folder")
        }
    };
    (media, entry.reference.library_id, unit.season, unit.episode)
}
fn assert_source(entry: &ContinueEntry, file: &domain::LibraryFile) {
    assert_eq!(entry.reference.library_id, file.library_id);
    assert_eq!(
        entry.reference.preferred_source,
        Some(domain::PhysicalSourceId {
            file_id: file.file_id,
            revision: file.version.revision,
        }),
        "selected source must belong to the selected coordinate's actual owner"
    );
}

struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    user: UserId,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        Self {
            dir,
            store,
            user: UserId::new(),
        }
    }
    fn library(&self, name: &str) -> LibraryId {
        self.library_kind(name, MediaKind::Tv)
    }
    fn library_kind(&self, name: &str, kind: MediaKind) -> LibraryId {
        let root = self.dir.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        self.store
            .create_library(kind, name, &[root.to_str().unwrap()], "everyone", true, &[])
            .unwrap()
            .id
            .parse()
            .unwrap()
    }
    fn media(&self, n: u64) -> MediaId {
        self.media_kind(n, MediaKind::Tv)
    }
    fn media_kind(&self, n: u64, kind: MediaKind) -> MediaId {
        let id = format!("00000000-0000-0000-0000-{n:012}").parse().unwrap();
        self.store
            .insert_media(&Media {
                id,
                kind,
                title: format!("Show {n}"),
                year: None,
                original_title: None,
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
                aliases: vec![],
            })
            .unwrap();
        id
    }
    fn own(&self, lib: LibraryId, media: MediaId, units: &[UnitKey]) -> domain::LibraryFile {
        let config = self.store.get_library(&lib.to_string()).unwrap().unwrap();
        let path = config.root_paths[0].join(format!("{media}.mkv"));
        std::fs::write(&path, b"fixture").unwrap();
        publish_fixture_file_units(
            &self.store,
            &FixtureFile {
                media_id: media,
                path: path.display().to_string(),
                ..Default::default()
            },
            units,
        )
        .unwrap()
    }
    fn progress(&self, media: MediaId, season: i32, episode: i32, now: i64) {
        self.store
            .upsert_unit(
                self.user,
                media,
                season,
                episode,
                30_000,
                None,
                None,
                Some(600_000),
                None,
                None,
                false,
                now,
            )
            .unwrap();
    }
    fn mark(&self, media: MediaId, scope: UnitScope, played: bool, now_ms: i64) {
        self.store
            .mutate_playback(
                media,
                &PlaybackMutation {
                    user_id: self.user,
                    scope,
                    played: Some(played),
                    favorite: None,
                    expected_generation: None,
                },
                now_ms,
            )
            .unwrap();
    }
}

#[test]
fn next_up_keeps_unstarted_and_cross_library_season_continuation() {
    let f = Fixture::new();
    let a = f.library("a");
    let b = f.library("b");
    let c = f.library("c");
    let continuing = f.media(1);
    let unstarted = f.media(2);
    f.own(a, continuing, &[UnitKey::new(1, 1)]);
    let continuing_source = f.own(b, continuing, &[UnitKey::new(2, 1)]);
    let unstarted_source = f.own(c, unstarted, &[UnitKey::new(1, 1)]);
    f.progress(continuing, 1, 1, 100);
    f.mark(continuing, UnitScope::Season(1), true, 200_000);
    let page = f
        .store
        .next_up_page(&query(f.user, &[a, b, c], 20, 0))
        .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.items.len(), 2);
    assert_source(&page.items[0], &continuing_source);
    assert_source(&page.items[1], &unstarted_source);
    assert_eq!(
        (
            target(&page.items[0]).0,
            target(&page.items[0]).1,
            target(&page.items[0]).2,
            target(&page.items[0]).3
        ),
        (continuing, b, 2, 1)
    );
    assert_eq!(
        (
            target(&page.items[1]).0,
            target(&page.items[1]).1,
            target(&page.items[1]).2,
            target(&page.items[1]).3
        ),
        (unstarted, c, 1, 1)
    );
    let restricted = f
        .store
        .next_up_page(&query(f.user, &[a, c], 20, 0))
        .unwrap();
    assert_eq!(restricted.total, 1);
    assert_eq!(restricted.items.len(), 1);
    assert_eq!(target(&restricted.items[0]).0, unstarted);
    f.store.begin_library_removal(&b.to_string()).unwrap();
    let hidden = f
        .store
        .next_up_page(&query(f.user, &[a, b, c], 20, 0))
        .unwrap();
    assert_eq!(
        hidden.items.len(),
        1,
        "active removal is hidden even if caller supplies id"
    );
    assert_eq!(hidden.total, 1);
    assert_eq!(target(&hidden.items[0]).0, unstarted);
}

#[test]
fn next_up_orders_by_activity_and_dedupes_before_paging() {
    let f = Fixture::new();
    let a = f.library("a");
    let b = f.library("b");
    let hidden = f.library("excluded");
    let low = f.media(1);
    let high = f.media(2);
    let newest = f.media(3);
    let excluded = f.media(4);
    for (id, clock) in [(low, 20), (high, 20), (newest, 30)] {
        f.own(a, id, &[UnitKey::new(1, 1), UnitKey::new(1, 2)]);
        f.progress(id, 1, 1, clock);
    }
    f.own(b, newest, &[UnitKey::new(1, 1), UnitKey::new(1, 2)]);
    f.own(hidden, excluded, &[UnitKey::new(1, 1)]);
    f.progress(excluded, 1, 1, 100);
    let all = f
        .store
        .next_up_page(&query(f.user, &[a, b], 20, 0))
        .unwrap();
    assert_eq!(
        all.items.iter().map(|r| target(r).0).collect::<Vec<_>>(),
        [newest, low, high]
    );
    assert_eq!(
        all.items
            .iter()
            .map(|r| r.last_played_at)
            .collect::<Vec<_>>(),
        [30, 20, 20]
    );
    let middle = f.store.next_up_page(&query(f.user, &[a, b], 1, 1)).unwrap();
    assert_eq!(all.total, 3);
    assert_eq!(middle.total, 3);
    assert_eq!(middle.items.len(), 1);
    assert_eq!(target(&middle.items[0]).0, low);
    let mut pinned_query = query(f.user, &[a, b], 1, 0);
    pinned_query.media_id = Some(high);
    pinned_query.season = Some(1);
    let pinned = f.store.next_up_page(&pinned_query).unwrap();
    assert_eq!(
        pinned.total, 1,
        "Media/season scope is applied before paging"
    );
    assert_eq!(target(&pinned.items[0]).0, high);
    pinned_query.offset = 99;
    let pinned_beyond = f.store.next_up_page(&pinned_query).unwrap();
    assert_eq!(pinned_beyond.total, 1);
    assert!(pinned_beyond.items.is_empty());
    let beyond = f
        .store
        .next_up_page(&query(f.user, &[a, b], 1, 99))
        .unwrap();
    assert_eq!(beyond.total, 3, "out-of-range pages preserve exact total");
    assert!(beyond.items.is_empty());
    assert!(
        f.store
            .next_up_page(&query(f.user, &[], 20, 0))
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        f.store
            .next_up_page(&query(UserId::new(), &[a, b], 1, 0))
            .unwrap()
            .items[0]
            .last_played_at
            == 0
    );
}

#[test]
fn resume_uses_selected_unit_activity_and_newest_effective_assertion() {
    let f = Fixture::new();
    let a = f.library("a");
    let b = f.library("b");
    let show = f.media(1);
    let second = f.media(2);
    let first_source = f.own(a, show, &[UnitKey::new(1, 1)]);
    let second_source = f.own(b, show, &[UnitKey::new(2, 1)]);
    f.own(a, second, &[UnitKey::new(1, 1)]);
    f.progress(show, 1, 1, 10);
    f.progress(show, 2, 1, 20);
    f.progress(second, 1, 1, 15);
    let page = f.store.resume_page(&query(f.user, &[a, b], 20, 0)).unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.items.len(), 2);
    assert_source(&page.items[0], &second_source);
    assert_eq!(
        (
            target(&page.items[0]).0,
            target(&page.items[0]).1,
            target(&page.items[0]).2,
            target(&page.items[0]).3,
            page.items[0].last_played_at
        ),
        (show, b, 2, 1, 20)
    );
    assert_eq!(target(&page.items[1]).0, second);
    let beyond = f.store.resume_page(&query(f.user, &[a, b], 1, 99)).unwrap();
    assert_eq!(beyond.total, 2, "out-of-range pages preserve exact total");
    assert!(beyond.items.is_empty());
    f.mark(show, UnitScope::Season(2), true, 30_000);
    let page = f.store.resume_page(&query(f.user, &[a, b], 20, 0)).unwrap();
    assert_eq!(
        (target(&page.items[0]).0, target(&page.items[1]).0),
        (second, show),
        "order by selected valid resume unit, not unrelated latest unit activity"
    );
    assert_eq!(
        (
            target(&page.items[1]).1,
            target(&page.items[1]).2,
            target(&page.items[1]).3,
            page.items[1].last_played_at
        ),
        (a, 1, 1, 10)
    );
    assert_source(&page.items[1], &first_source);
    f.mark(show, UnitScope::Whole, true, 40_000);
    assert_eq!(
        f.store
            .resume_page(&query(f.user, &[a, b], 20, 0))
            .unwrap()
            .items
            .len(),
        1
    );
    f.mark(
        show,
        UnitScope::Episode {
            season: 2,
            episode: 1,
        },
        false,
        50_000,
    );
    let restored = f.store.resume_page(&query(f.user, &[a, b], 1, 0)).unwrap();
    assert_eq!(restored.total, 2);
    assert_source(&restored.items[0], &second_source);
    assert_eq!(
        (
            target(&restored.items[0]).0,
            target(&restored.items[0]).1,
            target(&restored.items[0]).2
        ),
        (show, b, 2)
    );
    assert!(
        f.store
            .resume_page(&query(UserId::new(), &[a, b], 20, 0))
            .unwrap()
            .items
            .is_empty()
    );
}

#[test]
fn continuation_statement_count_does_not_follow_unrelated_episode_history() {
    let f = Fixture::new();
    let lib = f.library("public");
    for n in 1..=20 {
        let id = f.media(n);
        f.own(lib, id, &[UnitKey::new(1, 1), UnitKey::new(1, 2)]);
        f.progress(id, 1, 1, 1_000 + n as i64);
    }
    let historical = f.media(999);
    let units: Vec<_> = (1..=200).map(|e| UnitKey::new(1, e)).collect();
    f.own(lib, historical, &units);
    f.mark(historical, UnitScope::Whole, true, 9_000_000);
    for episode in 1..=10 {
        f.mark(
            historical,
            UnitScope::Episode { season: 1, episode },
            true,
            episode as i64,
        );
    }
    let measure = || {
        f.store.with_stats(StatsScope::Library, || {
            let next = f.store.next_up_page(&query(f.user, &[lib], 20, 0)).unwrap();
            let resume = f.store.resume_page(&query(f.user, &[lib], 20, 0)).unwrap();
            assert_eq!(next.items.len(), 20);
            assert_eq!(resume.items.len(), 20);
            assert_eq!(
                target(&next.items[0]).0,
                "00000000-0000-0000-0000-000000000020".parse().unwrap()
            );
            assert_eq!(target(&resume.items[0]).0, target(&next.items[0]).0);
        })
    };
    let (_, small) = measure();
    for episode in 11..=200 {
        f.mark(
            historical,
            UnitScope::Episode { season: 1, episode },
            true,
            episode as i64,
        );
    }
    let (_, large) = measure();
    assert!(
        small.statements > 0 && large.statements > 0,
        "measurement must be active"
    );
    assert_eq!(
        large.statements, small.statements,
        "no per-history or per-episode read: {small:?} -> {large:?}"
    );
    // Candidate cardinality, coverage and page cardinality are fixed here. This
    // deliberately does NOT claim that exact count / cross-library dedupe is O(page).
    assert!(
        large.vm_steps <= small.vm_steps + 128,
        "raw episode history must not be reduced on reads: {small:?} -> {large:?}"
    );
    assert!(
        large.fullscan_steps <= small.fullscan_steps,
        "{small:?} -> {large:?}"
    );
    assert_eq!(large.sort_operations, small.sort_operations);
    assert!(
        large.hydrated_rows <= 40,
        "page-only hydration, combined two 20-row pages: {large:?}"
    );
}

// Diagnostic seam must EXPLAIN the *same generated statements* used by public
// next_up_page/resume_page (same scope/visibility branch and bindings). A literal
// hand-written lookalike SELECT is not evidence about production execution.
// Root may adjust this diagnostic method's name once implementation is settled.
#[test]
fn next_up_and_resume_plans_use_actual_partial_indexes() {
    let f = Fixture::new();
    let lib = f.library("public");
    let show = f.media(1);
    f.own(lib, show, &[UnitKey::new(1, 1), UnitKey::new(1, 2)]);
    f.progress(show, 1, 1, 100);
    let (next, resume) = f
        .store
        .continue_watching_query_plans(&query(f.user, &[lib], 20, 0))
        .unwrap();
    assert!(
        next.iter().any(|row| row.contains("user_items_next")),
        "{next:?}"
    );
    assert!(
        resume.iter().any(|row| row.contains("user_items_resume")),
        "{resume:?}"
    );
    // Explain every actual count, ordered candidate stream and selected-page
    // source query, rather than a simplified diagnostic-only SELECT.
    eprintln!("NextUp production plans: {next:#?}; Resume production plans: {resume:#?}");
    for plan in [&next, &resume] {
        assert!(
            !plan.iter().any(|row| row.contains("TEMP B-TREE")),
            "continuation must page ordered indexed candidates without sorting the full set: {plan:?}"
        );
        assert!(
            !plan.iter().any(|row| row.contains("SCAN playback_units")),
            "{plan:?}"
        );
        assert!(
            !plan.iter().any(|row| row.contains("SCAN library_units")),
            "{plan:?}"
        );
    }
}

#[test]
fn resume_movie_uses_whole_unit_coordinates_and_is_not_next_up_tv() {
    let f = Fixture::new();
    let lib = f.library_kind("movies", MediaKind::Movie);
    let movie = f.media_kind(1, MediaKind::Movie);
    f.own(lib, movie, &[UnitKey::WHOLE]);
    f.progress(movie, -1, -1, 100);
    let page = f.store.resume_page(&query(f.user, &[lib], 20, 0)).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(
        (
            target(&page.items[0]).0,
            target(&page.items[0]).1,
            target(&page.items[0]).2,
            target(&page.items[0]).3,
            page.items[0].last_played_at
        ),
        (movie, lib, -1, -1, 100)
    );
    assert!(
        f.store
            .next_up_page(&query(f.user, &[lib], 20, 0))
            .unwrap()
            .items
            .is_empty()
    );
    f.mark(movie, UnitScope::Whole, true, 200_000);
    assert!(
        f.store
            .resume_page(&query(f.user, &[lib], 20, 0))
            .unwrap()
            .items
            .is_empty()
    );
}

#[test]
fn unavailable_episode_advances_next_up_and_resumes_an_older_available_unit() {
    let f = Fixture::new();
    let library = f.library("availability");
    let media = f.media(1);
    let first = f.own(library, media, &[UnitKey::new(1, 1)]);
    let second_path = first.path.with_file_name("second.mkv");
    std::fs::write(&second_path, b"second available episode").unwrap();
    let second = publish_fixture_file_units(
        &f.store,
        &FixtureFile {
            media_id: media,
            path: second_path.display().to_string(),
            ..Default::default()
        },
        &[UnitKey::new(1, 2)],
    )
    .unwrap();
    f.progress(media, 1, 1, 30);
    f.progress(media, 1, 2, 20);
    let scope = query(f.user, &[library], 20, 0);
    let before = f.store.resume_page(&scope).unwrap();
    assert_eq!(target(&before.items[0]), (media, library, 1, 1));
    let mut observations = f
        .store
        .watcher_presence_page(first.path.to_str().unwrap(), None, 1)
        .unwrap();
    assert_eq!(observations.len(), 1);
    observations[0].presence = domain::FilePresence::Missing;
    f.store
        .apply_ledger_presence(&observations, store::LedgerPresenceMode::Verify, 40)
        .unwrap();
    let next = f.store.next_up_page(&scope).unwrap();
    let resume = f.store.resume_page(&scope).unwrap();
    assert_eq!(target(&next.items[0]), (media, library, 1, 2));
    assert_eq!(target(&resume.items[0]), (media, library, 1, 2));
    assert_source(&next.items[0], &second);
    assert_source(&resume.items[0], &second);
    let untouched = f
        .store
        .next_up_page(&query(UserId::new(), &[library], 20, 0))
        .unwrap();
    assert_eq!(target(&untouched.items[0]), (media, library, 1, 2));
    assert_source(&untouched.items[0], &second);
    f.store.delete_files(library, &[second.file_id]).unwrap();
    assert!(f.store.next_up_page(&scope).unwrap().items.is_empty());
    assert!(f.store.resume_page(&scope).unwrap().items.is_empty());
}

#[test]
fn resume_season_scope_preserves_the_tied_unit_selected_without_that_filter() {
    let f = Fixture::new();
    let library = f.library("tied-partials");
    let media = f.media(1);
    f.own(library, media, &[UnitKey::new(1, 1), UnitKey::new(1, 2)]);
    f.progress(media, 1, 1, 20);
    f.progress(media, 1, 2, 20);
    let unfiltered = query(f.user, &[library], 20, 0);
    let before = f.store.resume_page(&unfiltered).unwrap();
    assert_eq!(before.total, 1);
    let mut filtered = unfiltered;
    filtered.season = Some(1);
    let after = f.store.resume_page(&filtered).unwrap();
    assert_eq!(after.total, 1);
    assert_eq!(
        target(&after.items[0]),
        target(&before.items[0]),
        "both partial units remain in scope, so filtering cannot change the selected unit"
    );
    assert_eq!(
        after.items[0].reference.preferred_source,
        before.items[0].reference.preferred_source
    );
}

#[test]
fn zero_activity_and_unstarted_streams_merge_before_offset_paging() {
    let f = Fixture::new();
    let a = f.library("a");
    let b = f.library("b");
    let ids = (1..=7).map(|n| f.media(n)).collect::<Vec<_>>();
    for id in &ids {
        f.own(a, *id, &[UnitKey::new(1, 1), UnitKey::new(1, 2)]);
        f.own(b, *id, &[UnitKey::new(1, 1)]);
    }
    // Alternate touched/untouched Media at the same clock. The two ordered
    // candidate streams must merge, rather than append after one another.
    for index in [0, 2, 4] {
        f.progress(ids[index], 1, 1, 0);
    }
    f.progress(ids[5], 1, 1, 20);
    f.mark(ids[6], UnitScope::Whole, true, 30_000);
    let expected = [ids[5], ids[0], ids[1], ids[2], ids[3], ids[4]];
    let mut observed = Vec::new();
    for offset in [0, 2, 4, 6] {
        let page = f
            .store
            .next_up_page(&query(f.user, &[a, b], 2, offset))
            .unwrap();
        assert_eq!(page.total, expected.len());
        observed.extend(page.items.iter().map(|row| target(row).0));
    }
    assert_eq!(observed, expected);
    let empty = f.store.next_up_page(&query(f.user, &[a, b], 0, 0)).unwrap();
    assert_eq!(empty.total, expected.len());
    assert!(empty.items.is_empty());
}

#[test]
fn scoped_continuation_plans_do_not_sort_candidates_or_sources() {
    let f = Fixture::new();
    let a = f.library("a");
    let b = f.library("b");
    let media = f.media(1);
    let source = f.own(a, media, &[UnitKey::new(1, 1), UnitKey::new(1, 2)]);
    f.own(b, media, &[UnitKey::new(2, 1)]);
    f.progress(media, 1, 1, 10);
    f.progress(media, 1, 2, 20);
    for parent in [
        domain::ProtocolParent::Root,
        domain::ProtocolParent::Library(a),
        domain::ProtocolParent::Series(media),
        domain::ProtocolParent::Season(media, 1),
        domain::ProtocolParent::Folder {
            root_id: source.root_id.clone(),
        },
        domain::ProtocolParent::Genre("Drama".into()),
        domain::ProtocolParent::Studio("Studio".into()),
    ] {
        let mut request = query(f.user, &[a, b], 1, 0);
        request.parent = parent;
        let (next, resume) = f.store.continue_watching_query_plans(&request).unwrap();
        for plans in [next, resume] {
            assert!(
                !plans.iter().any(|row| row.contains("TEMP B-TREE")),
                "{:?}: {plans:?}",
                request.parent
            );
        }
    }
}

#[test]
fn candidate_growth_does_not_turn_cross_library_deduplication_into_quadratic_work() {
    let f = Fixture::new();
    let a = f.library("a");
    let b = f.library("b");
    let populate = |start, end| {
        for n in start..=end {
            let media = f.media(n);
            for library in [a, b] {
                f.own(library, media, &[UnitKey::new(1, 1), UnitKey::new(1, 2)]);
            }
            f.progress(media, 1, 1, n as i64);
        }
    };
    let measure = |expected| {
        let (_, stats) = f.store.with_stats(StatsScope::Library, || {
            let request = query(f.user, &[a, b], 20, 0);
            for page in [
                f.store.next_up_page(&request).unwrap(),
                f.store.resume_page(&request).unwrap(),
            ] {
                assert_eq!(page.total, expected);
                assert_eq!(page.items.len(), 20);
                assert_eq!(
                    target(&page.items[0]).0,
                    format!("00000000-0000-0000-0000-{expected:012}")
                        .parse()
                        .unwrap()
                );
            }
        });
        stats
    };
    populate(1, 64);
    let small = measure(64);
    populate(65, 128);
    let large = measure(128);
    assert!(small.vm_steps > 0);
    assert!(
        large.vm_steps < small.vm_steps * 3,
        "doubling real candidates may increase count work linearly, never square it: {small:?} -> {large:?}"
    );
    assert_eq!(large.statements, small.statements);
    assert_eq!(large.sort_operations, 0, "{large:?}");
}
