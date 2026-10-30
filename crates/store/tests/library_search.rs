//! Public Library search, projection lifecycle, and query-work regressions.
use domain::{LibraryId, Media, MediaId, MediaKind};
use store::test_support::{FixtureFile, publish_fixture_file};
use store::{Store, stats::StatsScope};

struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        Self { dir, store }
    }
    fn library(&self, name: &str) -> LibraryId {
        let root = self.dir.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        self.store
            .create_library(
                MediaKind::Movie,
                name,
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
            .parse()
            .unwrap()
    }
    fn publish(&self, lib: LibraryId, title: &str, actor: &str) -> MediaId {
        let media = MediaId::new();
        self.store
            .insert_media(&Media {
                id: media,
                kind: MediaKind::Movie,
                title: title.into(),
                year: None,
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        self.publish_existing(lib, media, title, actor);
        media
    }
    fn publish_existing(&self, lib: LibraryId, media: MediaId, title: &str, actor: &str) {
        let config = self.store.get_library(&lib.to_string()).unwrap().unwrap();
        let file = config.root_paths[0].join(format!("{media}.mkv"));
        std::fs::write(&file, b"movie").unwrap();
        std::fs::write(
            file.with_extension("nfo"),
            format!("<movie><title>{title}</title><actor><name>{actor}</name></actor></movie>"),
        )
        .unwrap();
        publish_fixture_file(
            &self.store,
            &FixtureFile {
                media_id: media,
                path: file.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
    }
    fn ids(&self, libs: &[LibraryId], term: &str) -> Vec<MediaId> {
        self.store
            .search_library_items(libs, term, None, None)
            .unwrap()
            .items
            .into_iter()
            .map(|hit| hit.media_id)
            .collect()
    }
}

#[test]
fn search_matches_title_and_people_substrings() {
    let f = Fixture::new();
    let lib = f.library("public");
    let title = f.publish(lib, "追寻流浪地球电影", "John Fitzgerald");
    assert_eq!(f.ids(&[lib], "流浪地"), [title]);
    let hit = f
        .store
        .search_library_items(&[lib], "FITZGER", None, None)
        .unwrap();
    assert_eq!(hit.items.len(), 1);
    assert_eq!(hit.items[0].media_id, title);
    assert_eq!(hit.items[0].matched_by, "person");
}

#[test]
fn search_shorter_than_three_chars_falls_back_to_title_prefix() {
    let f = Fixture::new();
    let lib = f.library("public");
    let prefix = f.publish(lib, "流浪地球", "Al Pacino");
    f.publish(lib, "寻找流浪", "John Doe");
    let latin = f.publish(lib, "Alien", "Zoe Saldana");
    assert_eq!(f.ids(&[lib], "流浪"), [prefix]);
    assert_eq!(f.ids(&[lib], "流"), [prefix]);
    assert_eq!(
        f.ids(&[lib], "al"),
        [latin],
        "short fallback is title prefix, never actor substring"
    );
}

#[test]
fn search_literal_fts_operators_are_user_text() {
    let f = Fixture::new();
    let lib = f.library("public");
    let id = f.publish(lib, "Literal OR (test) quote\" here", "Nobody");
    assert_eq!(f.ids(&[lib], "or (test)"), [id]);
    assert_eq!(f.ids(&[lib], "quote\" here"), [id]);
    assert!(f.ids(&[lib], "missing OR here").is_empty());
}

#[test]
fn search_respects_library_visibility_and_active_removal() {
    let f = Fixture::new();
    let visible = f.library("visible");
    let hidden = f.library("hidden");
    let wanted = f.publish(visible, "Needle One", "Nobody");
    f.publish(hidden, "Needle Two", "Nobody");
    assert_eq!(f.ids(&[visible], "needle"), [wanted]);
    f.store.begin_library_removal(&visible.to_string()).unwrap();
    assert!(f.ids(&[visible], "needle").is_empty());
}

#[test]
fn search_deduplicates_shared_media_before_two_column_cursor_paging() {
    let f = Fixture::new();
    let a = f.library("a");
    let b = f.library("b");
    let shared = f.publish(a, "Needle A", "Nobody");
    f.publish_existing(b, shared, "Needle A", "Nobody");
    let next = f.publish(a, "Needle B", "Nobody");
    let first = f
        .store
        .search_library_items(&[a, b], "needle", Some(1), None)
        .unwrap();
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].media_id, shared);
    let second = f
        .store
        .search_library_items(&[a, b], "needle", Some(1), first.next_cursor.as_deref())
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].media_id, next);
    assert!(second.next_cursor.is_none());
    assert_eq!(f.ids(&[a, b], "needle"), [shared, next]);
}

#[test]
fn search_work_does_not_grow_with_unrelated_cards_or_people() {
    let f = Fixture::new();
    let lib = f.library("public");
    let target = f.publish(lib, "Needle Film", "Target Performer");
    let measure = || {
        f.store.with_stats(StatsScope::Library, || {
            assert_eq!(f.ids(&[lib], "needle"), [target]);
            assert_eq!(f.ids(&[lib], "performer"), [target]);
            assert_eq!(f.ids(&[lib], "ne"), [target]);
        })
    };
    let (_, small) = measure();
    for i in 0..160 {
        f.publish(
            lib,
            &format!("Unrelated {i}"),
            &format!("Different Person {i}"),
        );
    }
    let (_, large) = measure();
    assert!(
        large.fullscan_steps <= small.fullscan_steps + 8,
        "{small:?} -> {large:?}"
    );
    assert!(
        large.vm_steps <= small.vm_steps + 800,
        "{small:?} -> {large:?}"
    );
}

use domain::{
    EntityScope, ProtocolItemQuery, ProtocolItemType, ProtocolParent, TypedItemId, User, UserId,
    UserRole,
};
use rusqlite::params;

impl Fixture {
    fn raw(&self, lib: LibraryId, title: &str) -> (MediaId, domain::LibraryFile) {
        let kind = self
            .store
            .get_library(&lib.to_string())
            .unwrap()
            .unwrap()
            .kind;
        let id = MediaId::new();
        self.store
            .insert_media(&Media {
                id,
                kind,
                title: title.into(),
                year: None,
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let file = self.raw_existing(lib, id);
        (id, file)
    }
    fn raw_existing(&self, lib: LibraryId, id: MediaId) -> domain::LibraryFile {
        let config = self.store.get_library(&lib.to_string()).unwrap().unwrap();
        let file = config.root_paths[0].join(format!("{id}.mkv"));
        std::fs::write(&file, b"movie").unwrap();
        publish_fixture_file(
            &self.store,
            &FixtureFile {
                media_id: id,
                path: file.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap()
    }
    fn fields(&self, media: MediaId, locale: &str, json: serde_json::Value) -> i64 {
        let revision = self
            .store
            .get_metadata_overrides(media, EntityScope::Work, locale)
            .unwrap()
            .map_or(0, |(_, revision)| revision);
        self.store
            .set_metadata_overrides(
                media,
                EntityScope::Work,
                locale,
                &serde_json::from_value(json).unwrap(),
                revision,
            )
            .unwrap()
    }
    fn user(&self) -> UserId {
        let id = UserId::new();
        self.store
            .insert_user(&User {
                id,
                login: format!("search-{id}"),
                enabled: true,
                role: UserRole::Admin,
            })
            .unwrap();
        id
    }
    fn integrity(&self, library: LibraryId, repair: bool, operation: &str) {
        self.store
            .begin_library_integrity(library, repair, operation, 1000)
            .unwrap();
        for step in 1..100 {
            let state = self
                .store
                .advance_library_operation(operation, 1000 + step, 1)
                .unwrap();
            if state.state == "succeeded" {
                return;
            }
            assert_ne!(state.state, "failed", "{state:?}");
        }
        panic!("finite fixture integrity did not terminate");
    }
}

#[test]
fn search_metadata_replacement_removes_old_title_and_actor_postings() {
    let f = Fixture::new();
    let lib = f.library("public");
    let (id, _) = f.raw(lib, "Identity Film");
    f.fields(
        id,
        "zh-CN",
        serde_json::json!({
            "title":{"state":"value","value":"Obsolete Quantum"},
            "cast":{"state":"value","value":[{"name":"Retired Performer"}]}
        }),
    );
    assert_eq!(f.ids(&[lib], "obsolete"), [id]);
    assert_eq!(f.ids(&[lib], "retired"), [id]);
    f.fields(
        id,
        "zh-CN",
        serde_json::json!({
            "title":{"state":"value","value":"Current Horizon"},
            "cast":{"state":"value","value":[{"name":"Current Performer"}]}
        }),
    );
    assert_eq!(f.ids(&[lib], "horizon"), [id]);
    assert_eq!(f.ids(&[lib], "current performer"), [id]);
    assert!(f.ids(&[lib], "obsolete").is_empty());
    assert!(f.ids(&[lib], "retired").is_empty());
    f.integrity(lib, false, "metadata-verify");
    assert!(
        !f.store
            .library_integrity_report("metadata-verify")
            .unwrap()
            .has_issues()
    );
}

#[test]
fn search_missing_delete_and_reopen_never_return_phantom_postings() {
    let f = Fixture::new();
    let lib = f.library("public");
    let (id, file) = f.raw(lib, "Needle Original");
    assert_eq!(f.ids(&[lib], "needle"), [id]);
    let mut observations = f
        .store
        .watcher_presence_page(file.path.to_str().unwrap(), None, 1)
        .unwrap();
    assert_eq!(observations.len(), 1);
    observations[0].presence = domain::FilePresence::Missing;
    f.store
        .apply_ledger_presence(&observations, store::LedgerPresenceMode::Verify, 20)
        .unwrap();
    assert!(
        f.ids(&[lib], "needle").is_empty(),
        "missing publication is not searchable"
    );
    let mut observations = f
        .store
        .watcher_presence_page(file.path.to_str().unwrap(), None, 1)
        .unwrap();
    observations[0].presence = domain::FilePresence::Present;
    f.store
        .apply_ledger_presence(&observations, store::LedgerPresenceMode::Verify, 30)
        .unwrap();
    assert_eq!(f.ids(&[lib], "needle"), [id]);
    f.store.delete_files(lib, &[file.file_id]).unwrap();
    assert!(f.ids(&[lib], "needle").is_empty());
    let (new_id, _) = f.raw(lib, "Replacement Comet");
    assert_eq!(f.ids(&[lib], "comet"), [new_id]);
    let Fixture { dir, store } = f;
    drop(store);
    let reopened = Store::open(dir.path().join("data")).unwrap();
    assert!(
        reopened
            .search_library_items(&[lib], "needle", None, None)
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        reopened
            .search_library_items(&[lib], "comet", None, None)
            .unwrap()
            .items[0]
            .media_id,
        new_id
    );
}

#[test]
fn search_postings_roll_back_with_failed_multi_library_metadata_reduction() {
    let f = Fixture::new();
    let a = f.library("first");
    let b = f.library("second");
    let (id, _) = f.raw(a, "Stable Identity");
    f.raw_existing(b, id);
    f.fields(
        id,
        "zh-CN",
        serde_json::json!({"title":{"state":"value","value":"Original Beacon"}}),
    );
    let db = rusqlite::Connection::open(f.dir.path().join("data/library.db")).unwrap();
    // A finite storage fault in the second card update makes the public metadata
    // write roll back after another card may already have settled its FTS entry.
    let reject = if a.to_string() > b.to_string() { a } else { b };
    db.execute_batch(&format!("CREATE TRIGGER reject_second_search_card BEFORE UPDATE ON library_items WHEN NEW.library_id='{}' AND NEW.media_id='{}' AND NEW.title='Rejected Horizon' BEGIN SELECT RAISE(ABORT,'finite search fixture failure'); END;",reject,id)).unwrap();
    let fields = serde_json::from_value(
        serde_json::json!({"title":{"state":"value","value":"Rejected Horizon"}}),
    )
    .unwrap();
    assert!(
        f.store
            .set_metadata_overrides(id, EntityScope::Work, "zh-CN", &fields, 1)
            .is_err()
    );
    assert_eq!(f.ids(&[a, b], "beacon"), [id]);
    assert!(f.ids(&[a, b], "horizon").is_empty());
    assert_eq!(
        f.store
            .get_metadata_overrides(id, EntityScope::Work, "zh-CN")
            .unwrap()
            .unwrap()
            .1,
        1
    );
    db.execute_batch("DROP TRIGGER reject_second_search_card")
        .unwrap();
    f.integrity(a, false, "rollback-verify-a");
    f.integrity(b, false, "rollback-verify-b");
    assert!(
        !f.store
            .library_integrity_report("rollback-verify-a")
            .unwrap()
            .has_issues()
    );
    assert!(
        !f.store
            .library_integrity_report("rollback-verify-b")
            .unwrap()
            .has_issues()
    );
}

#[test]
fn search_integrity_detects_and_repairs_missing_stale_and_orphan_postings() {
    for damage in ["missing", "wrong-tokens", "orphan"] {
        let f = Fixture::new();
        let lib = f.library("public");
        let (id, _) = f.raw(lib, "Needle Original");
        let db = rusqlite::Connection::open(f.dir.path().join("data/library.db")).unwrap();
        let rowid: i64 = db
            .query_row(
                "SELECT rowid FROM library_items WHERE library_id=?1 AND media_id=?2",
                params![lib.to_string(), id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        match damage {
            "missing" => {
                db.execute("DELETE FROM library_search_fts WHERE rowid=?1", [rowid])
                    .unwrap();
            }
            "wrong-tokens" => {
                db.execute("DELETE FROM library_search_fts WHERE rowid=?1", [rowid])
                    .unwrap();
                db.execute("INSERT INTO library_search_fts(rowid,search_text,people) VALUES(?1,'obsolete phantom','wrong performer')", [rowid]).unwrap();
            }
            _ => {
                db.execute("INSERT INTO library_search_fts(rowid,search_text,people) VALUES(?1,'obsolete phantom','wrong performer')", [rowid + 1_000_000]).unwrap();
            }
        }
        // Damage storage only. Each independent fixture forces the public check
        // to detect this exact fault; a missing row cannot mask wrong-token failure.
        f.integrity(lib, false, "fts-damage-check");
        assert!(
            f.store
                .library_integrity_report("fts-damage-check")
                .unwrap()
                .has_issues(),
            "{damage}"
        );
        f.integrity(lib, true, "fts-damage-repair");
        assert_eq!(f.ids(&[lib], "needle"), [id], "{damage}");
        assert!(f.ids(&[lib], "phantom").is_empty(), "{damage}");
        assert!(f.ids(&[lib], "wrong performer").is_empty(), "{damage}");
        f.integrity(lib, false, "fts-damage-verify");
        assert!(
            !f.store
                .library_integrity_report("fts-damage-verify")
                .unwrap()
                .has_issues(),
            "{damage}"
        );
    }
}

fn protocol_query(user: UserId, library: LibraryId, term: &str) -> ProtocolItemQuery {
    ProtocolItemQuery {
        user_id: user,
        parent: ProtocolParent::Library(library),
        series_id: None,
        season: None,
        person_ids: vec![],
        studio_ids: vec![],
        search_term: Some(term.into()),
        genres: vec![],
        genre_ids: vec![],
        filters: vec![],
        is_favorite: None,
        is_played: None,
        include_types: vec![ProtocolItemType::Movie],
        recursive: true,
        is_folder: None,
        sort: vec![],
        offset: 0,
        limit: 20,
    }
}

#[test]
fn search_fts_candidates_preserve_distinct_self_protocol_and_hint_predicates() {
    let f = Fixture::new();
    let user = f.user();
    let lib = f.library("public");
    let (id, _) = f.raw(lib, "Identity Film");
    f.fields(
        id,
        "zh-CN",
        serde_json::json!({
            "title":{"state":"value","value":"Chosen Needle"},
            "original_title":{"state":"value","value":"Original Galaxy"},
            "overview":{"state":"value","value":"Overview Beacon"},
            "cast":{"state":"value","value":[{"name":"Person Marker"}]}
        }),
    );
    f.fields(
        id,
        "en-US",
        serde_json::json!({"original_title":{"state":"value","value":"Foreign Secret"}}),
    );
    for term in ["needle", "galaxy", "beacon"] {
        let page = f
            .store
            .protocol_item_page(&protocol_query(user, lib, term))
            .unwrap();
        assert_eq!(page.total, 1, "selected-locale Work SearchTerm: {term}");
        assert_eq!(page.items[0].item, TypedItemId::Work(id));
    }
    for term in ["person marker", "foreign secret"] {
        assert_eq!(
            f.ids(&[lib], term),
            [id],
            "Self includes people and all persisted locale text"
        );
        assert_eq!(
            f.store
                .protocol_item_page(&protocol_query(user, lib, term))
                .unwrap()
                .total,
            0,
            "Protocol Work does not widen to actor/all-locale FTS candidates"
        );
    }
    for term in ["needle", "galaxy"] {
        let hints = f.store.protocol_search_hints(user, term, 0, 20).unwrap();
        assert_eq!(hints.total, 1);
        assert_eq!(hints.items[0].item, TypedItemId::Work(id));
    }
    for term in ["beacon", "person marker", "foreign secret"] {
        assert_eq!(
            f.store
                .protocol_search_hints(user, term, 0, 20)
                .unwrap()
                .total,
            0,
            "Hints remain selected title/original-only: {term}"
        );
    }
}

#[test]
fn search_hints_work_does_not_grow_with_unrelated_cards() {
    let f = Fixture::new();
    let user = f.user();
    let lib = f.library("public");
    let target = f.publish(lib, "Needle Film", "Nobody");
    let measure = || {
        f.store.with_stats(StatsScope::Library, || {
            let page = f.store.protocol_search_hints(user, "needle", 0, 1).unwrap();
            assert_eq!(page.total, 1);
            assert_eq!(page.items[0].item, TypedItemId::Work(target));
        })
    };
    let (_, small) = measure();
    for i in 0..160 {
        f.publish(
            lib,
            &format!("Unrelated {i}"),
            &format!("Different Person {i}"),
        );
    }
    let (_, large) = measure();
    assert!(small.vm_steps > 0 && large.vm_steps > 0);
    assert!(
        large.fullscan_steps <= small.fullscan_steps + 8,
        "{small:?} -> {large:?}"
    );
    assert!(
        large.vm_steps <= small.vm_steps + 800,
        "{small:?} -> {large:?}"
    );
}

#[test]
fn search_nfo_refresh_replaces_postings_in_publication_transaction() {
    let f = Fixture::new();
    let lib = f.library("public");
    let id = f.publish(lib, "Obsolete Lantern", "Retired Performer");
    assert_eq!(f.ids(&[lib], "lantern"), [id]);
    f.publish_existing(lib, id, "Current Horizon", "Current Performer");
    assert_eq!(f.ids(&[lib], "horizon"), [id]);
    assert_eq!(f.ids(&[lib], "current performer"), [id]);
    assert!(f.ids(&[lib], "lantern").is_empty());
    assert!(f.ids(&[lib], "retired").is_empty());
    f.integrity(lib, false, "nfo-republish-verify");
    assert!(
        !f.store
            .library_integrity_report("nfo-republish-verify")
            .unwrap()
            .has_issues()
    );
}

#[test]
fn protocol_episode_search_does_not_require_a_work_fts_match() {
    let f = Fixture::new();
    let user = f.user();
    let root = f.dir.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    let lib = f
        .store
        .create_library(
            MediaKind::Tv,
            "TV",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let lib: LibraryId = lib.id.parse().unwrap();
    let (id, file) = f.raw(lib, "Work Outer");
    std::fs::write(file.path.with_extension("nfo"), "<episodedetails><season>1</season><episode>1</episode><title>Episode Beacon</title><plot>Episode Horizon</plot></episodedetails>").unwrap();
    publish_fixture_file(
        &f.store,
        &FixtureFile {
            media_id: id,
            path: file.path.display().to_string(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    for term in ["beacon", "horizon"] {
        let mut request = protocol_query(user, lib, term);
        request.include_types = vec![ProtocolItemType::Episode];
        let page = f.store.protocol_item_page(&request).unwrap();
        assert_eq!(
            page.total, 1,
            "episode-only {term} cannot be gated by work FTS"
        );
        assert_eq!(
            page.items[0].item,
            TypedItemId::Unit {
                media: id,
                key: domain::UnitKey::new(1, 1)
            }
        );
    }
    let mut work_term = protocol_query(user, lib, "work outer");
    work_term.include_types = vec![ProtocolItemType::Episode];
    assert_eq!(
        f.store.protocol_item_page(&work_term).unwrap().total,
        0,
        "episode SearchTerm cannot widen to work text"
    );
}

#[test]
fn search_results_survive_full_vacuum_and_fresh_reader_reopen() {
    let f = Fixture::new();
    let lib = f.library("public");
    let (discard, discard_file) = f.raw(lib, "Discard Old");
    let (wanted, _) = f.raw(lib, "Needle Stable");
    f.store.delete_files(lib, &[discard_file.file_id]).unwrap();
    assert!(f.ids(&[lib], "discard").is_empty());
    let Fixture { dir, store } = f;
    drop(store);
    let db = rusqlite::Connection::open(dir.path().join("data/library.db")).unwrap();
    db.execute_batch("VACUUM").unwrap();
    drop(db);
    let reopened = Store::open(dir.path().join("data")).unwrap();
    let hits = reopened
        .search_library_items(&[lib], "needle", None, None)
        .unwrap();
    assert_eq!(hits.items.len(), 1);
    assert_eq!(hits.items[0].media_id, wanted);
    assert!(
        reopened
            .search_library_items(&[lib], "discard", None, None)
            .unwrap()
            .items
            .iter()
            .all(|hit| hit.media_id != discard)
    );
}

// Append to library_search.rs only after root elects this diagnostic seam.
// Proposed API follows page_read_query_plans and is test-support-only:
// Store::search_library_items_query_plans(libraries, keyword, limit, cursor)
//   -> Result<Vec<String>, StoreError>
// IMPORTANT: it must call the exact production query+bind builder, prefixing only
// EXPLAIN QUERY PLAN. Tests never maintain another copy of production SQL.

#[test]
fn search_plan_uses_fts_and_rowid_seek() {
    let f = Fixture::new();
    let lib = f.library("public");
    f.publish(lib, "Needle Film", "Some Performer");
    let plans = f
        .store
        .search_library_items_query_plans(&[lib], "needle", Some(10), None)
        .unwrap();
    assert!(plans.iter().any(|plan| plan.contains("library_search_fts") && plan.contains("VIRTUAL TABLE INDEX")), "{plans:?}");
    assert!(
        plans
            .iter()
            .any(|plan| plan.contains("SEARCH i USING INTEGER PRIMARY KEY")),
        "FTS candidate rowids must seek cards: {plans:?}"
    );
    for plan in &plans {
        let detail = plan
            .split_once(": ")
            .map_or(plan.as_str(), |(_, detail)| detail);
        let upper = detail.to_ascii_uppercase();
        assert!(
            !upper.starts_with("SCAN I ") && upper != "SCAN I",
            "large library_items alias: {plans:?}"
        );
        assert!(
            !upper.starts_with("SCAN P ") && upper != "SCAN P",
            "large people alias: {plans:?}"
        );
        assert!(
            !upper.contains("SCAN LIBRARY_ITEMS")
                && !upper.contains("SCAN LIBRARY_PROTOCOL_PEOPLE"),
            "{plans:?}"
        );
    }
    // Sorting the finite matched set is legitimate: FTS postings are not title-
    // ordered. Banning all TEMP B-TREE would contradict exact title ordering.
}

#[test]
fn search_short_prefix_plan_uses_range_seek_without_large_table_scan() {
    let f = Fixture::new();
    let lib = f.library("public");
    f.publish(lib, "Alien", "Nobody");
    let plans = f
        .store
        .search_library_items_query_plans(&[lib], "al", Some(10), None)
        .unwrap();
    assert!(
        plans
            .iter()
            .any(|plan| plan.contains("SEARCH i USING INDEX")
                && plan.contains("protocol_title_key>?")
                && plan.contains("protocol_title_key<?")),
        "{plans:?}"
    );
    for plan in &plans {
        let detail = plan
            .split_once(": ")
            .map_or(plan.as_str(), |(_, detail)| detail);
        let upper = detail.to_ascii_uppercase();
        assert!(
            !upper.starts_with("SCAN I ") && upper != "SCAN I",
            "{plans:?}"
        );
        assert!(
            !upper.contains("SCAN LIBRARY_ITEMS")
                && !upper.contains("SCAN LIBRARY_PROTOCOL_PEOPLE"),
            "{plans:?}"
        );
    }
}
