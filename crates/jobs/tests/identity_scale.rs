use jobs::{JobIdentity, JobKind, Queue};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn exact_identity_lookup_and_media_rewrite_work_do_not_grow_with_unrelated_history() {
    let tmp = tempfile::tempdir().unwrap();
    let q = Queue::open(tmp.path().join("jobs.db")).unwrap();
    let from = domain::MediaId::new();
    let to = domain::MediaId::new();
    let counter = Arc::new(AtomicUsize::new(0));
    let observed = counter.clone();
    q.with_connection(|c| {
        c.progress_handler(
            100,
            Some(move || {
                observed.fetch_add(1, Ordering::Relaxed);
                false
            }),
        )
    });
    let measure = || {
        counter.store(0, Ordering::Relaxed);
        assert!(
            q.jobs_for_identity(JobIdentity::global(JobKind::Transfer), 1)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            q.rewrite_media_refs(&from.to_string(), &to.to_string())
                .unwrap(),
            0
        );
        counter.load(Ordering::Relaxed)
    };
    let before = measure();
    q.with_connection(|c| c.execute_batch("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<10000) INSERT INTO jobs(id,kind,scope_key,payload,status,run_after) SELECT printf('%08x-0000-0000-0000-000000000000',x),'scrape','global','{}','running',x FROM n;")).unwrap();
    let after = measure();
    assert!(
        after <= before + 20,
        "unrelated active history inflated VM work: {before} -> {after}"
    );
}
