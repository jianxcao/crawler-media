//! Proposed MissingFile fields: file_id,media_id,title,path,missing_at,units.
#[path = "support/task10_fixture.rs"]
mod fixture;
use domain::{FilePresence, UnitKey};
use fixture::Fixture;
use store::stats::StatsScope;

#[test]
fn missing_files_is_library_scoped_and_keyset_complete() {
    let f = Fixture::new();
    let a = f.library("a");
    let b = f.library("b");
    let mut expected = vec![];
    for n in 1..=5 {
        let media = f.media(n);
        let file = f.publish(a, media, &format!("{n}.mkv"), &[UnitKey::WHOLE]);
        f.presence(&file, FilePresence::Missing, 100);
        expected.push((file.file_id, media, format!("Movie {n}")));
    }
    for n in 10..=25 {
        let media = f.media(n);
        let file = f.publish(b, media, &format!("{n}.mkv"), &[UnitKey::WHOLE]);
        f.presence(&file, FilePresence::Missing, 1);
    }
    expected.sort_by_key(|row| row.0.to_string());
    let mut after = None;
    let mut seen = vec![];
    loop {
        let page = f.store.missing_files_page(a, after, 2).unwrap();
        assert!(page.len() <= 2);
        if page.is_empty() {
            break;
        }
        for row in &page {
            assert_eq!(row.missing_at, 100);
            assert_eq!(
                row.title,
                expected.iter().find(|e| e.1 == row.media_id).unwrap().2
            );
            seen.push((row.file_id, row.media_id, row.title.clone()));
        }
        let last = page.last().unwrap();
        after = Some((last.missing_at, last.file_id));
    }
    assert_eq!(seen, expected);
    let plans = f.store.missing_files_query_plans(a, None, 2).unwrap();
    assert!(
        plans.iter().any(|p| p.contains("files_missing")),
        "{plans:?}"
    );
    assert!(
        !plans.iter().any(|p| p.contains("TEMP B-TREE")),
        "{plans:?}"
    );
    assert!(
        !plans
            .iter()
            .any(|p| p.starts_with("SCAN library_files") || p.starts_with("SCAN m")),
        "{plans:?}"
    );
    let (page, stats) = f.store.with_stats(StatsScope::Library, || {
        f.store.missing_files_page(a, None, 2).unwrap()
    });
    assert!(stats.statements > 0);
    // SQLite scans the already bounded materialized physical-file page. This
    // counter includes that tiny scan, so it must not be confused with a base
    // ledger scan; the actual plan above checks the source access separately.
    assert!(
        stats.fullscan_steps <= page.len().saturating_sub(1) as u64,
        "{stats:?}"
    );
}

#[test]
fn missing_page_work_is_bounded_as_unrelated_and_present_files_grow() {
    let f = Fixture::new();
    let a = f.library("a");
    let b = f.library("b");
    let media = f.media(1);
    for n in 0..2 {
        let file = f.publish(a, media, &format!("missing-{n}.mkv"), &[UnitKey::WHOLE]);
        f.presence(&file, FilePresence::Missing, 100);
    }
    let grow = |from, to| {
        for n in from..to {
            f.publish(a, media, &format!("present-{n}.mkv"), &[UnitKey::WHOLE]);
            let file = f.publish(b, media, &format!("unrelated-{n}.mkv"), &[UnitKey::WHOLE]);
            f.presence(&file, FilePresence::Missing, 1);
        }
    };
    grow(0, 100);
    let measure = || {
        f.store
            .with_stats(StatsScope::Library, || {
                let page = f.store.missing_files_page(a, None, 2).unwrap();
                assert_eq!(page.len(), 2);
                assert!(page.iter().all(|file| file.missing_at == 100));
            })
            .1
    };
    let small = measure();
    grow(100, 200);
    let large = measure();
    assert!(small.vm_steps > 0);
    assert!(
        large.vm_steps <= small.vm_steps + 30,
        "{small:?} -> {large:?}"
    );
    assert_eq!(large.statements, small.statements);
    assert_eq!(large.fullscan_steps, small.fullscan_steps);
}
#[test]
fn missing_pack_pagination_keeps_every_owned_unit_and_restore_removes_it() {
    let f = Fixture::new();
    let lib = f.library("a");
    let media = f.media(1);
    let units: Vec<_> = (1..=30).map(|e| UnitKey::new(1, e)).collect();
    let file = f.publish(lib, media, "pack.mkv", &units);
    f.presence(&file, FilePresence::Missing, 100);
    let page = f.store.missing_files_page(lib, None, 1).unwrap();
    assert_eq!(
        page.len(),
        1,
        "limit applies to physical files, not expanded units"
    );
    assert_eq!(page[0].units, units);
    assert_eq!(page[0].file_id, file.file_id);
    f.presence(&file, FilePresence::Present, 101);
    assert!(f.store.missing_files_page(lib, None, 1).unwrap().is_empty());
}
#[test]
fn missing_paths_keep_case_sensitive_identity_and_display_spelling() {
    let f = Fixture::new();
    let lib = f.library("a");
    let media = f.media(1);
    // Distinct recorded paths must remain distinct even on a case-insensitive
    // development filesystem. File bytes are irrelevant after publication.
    let upper = f.publish(lib, media, "Movie.mkv", &[UnitKey::WHOLE]);
    let lower = f.publish(lib, media, "movie.mkv", &[UnitKey::WHOLE]);
    assert_ne!(upper.file_id, lower.file_id);
    assert_ne!(
        f.store.recorded_path_key(upper.path.to_str().unwrap()),
        f.store.recorded_path_key(lower.path.to_str().unwrap())
    );
    f.presence(&upper, FilePresence::Missing, 100);
    let page = f.store.missing_files_page(lib, None, 20).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].file_id, upper.file_id);
    assert_eq!(page[0].path, upper.path);
}
