//! Persistent catalog contracts retain real subject/credits and typed errors.
use media::{CatalogGet, Douban, TmdbError};
use std::sync::{Arc, Mutex};

const NOW: i64 = 1_000_000;
const SUBJECT: &str = r#"{"id":"1292720","title":"Forrest Gump","type":"movie","intro":"Complete subject overview","rating":{"value":9.5},"genres":["Drama"],"pic":{"normal":"https://example.invalid/poster.jpg"}}"#;
const CREDITS: &str = r#"{"actors":[{"id":"1054450","name":"Tom Hanks","roles":["Forrest Gump"],"avatar":{"normal":"https://example.invalid/actor.jpg"}}],"directors":[{"id":"1044965","name":"Robert Zemeckis"}]}"#;
#[derive(Clone)]
enum Reply {
    Present,
    Failure,
    Missing,
    MalformedSubject,
    MalformedCredits,
}
#[derive(Clone)]
struct Source(Arc<Mutex<Reply>>);
impl CatalogGet for Source {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        match &*self.0.lock().unwrap() {
            Reply::Failure => Err(TmdbError::Http("transport unavailable".into())),
            Reply::Missing => Err(TmdbError::NotFound(path.into())),
            Reply::MalformedSubject if !path.contains("/celebrities") => {
                Ok(r#"{"error":"temporary denial"}"#.into())
            }
            Reply::MalformedCredits if path.contains("/celebrities") => {
                Ok(r#"{"error":"temporary denial"}"#.into())
            }
            Reply::Present | Reply::MalformedSubject | Reply::MalformedCredits => {
                if path.contains("/celebrities") {
                    Ok(CREDITS.into())
                } else {
                    let id = path
                        .split("/movie/")
                        .nth(1)
                        .and_then(|tail| tail.split(['/', '?']).next())
                        .unwrap_or("1292720");
                    let mut subject: serde_json::Value = serde_json::from_str(SUBJECT).unwrap();
                    subject["id"] = id.into();
                    Ok(subject.to_string())
                }
            }
        }
    }
}
#[test]
fn douban_subject_and_cast_use_persistent_cache() {
    let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let db = dir.path().join("cache.db");
    let source = Source(Arc::new(Mutex::new(Reply::Present)));
    let douban = Douban::new_at(source.clone(), &db, NOW).unwrap();
    let cold = douban.subject_with_credits("1292720").unwrap();
    assert_eq!(cold.subject["title"], "Forrest Gump");
    assert_eq!(cold.celebrities["actors"][0]["name"], "Tom Hanks");
    assert_eq!(cold.celebrities["directors"][0]["name"], "Robert Zemeckis");
    drop(douban);
    *source.0.lock().unwrap() = Reply::Failure;
    let reopened = Douban::new_at(source, &db, NOW + 1).unwrap();
    let fresh = reopened.subject_with_credits("1292720").unwrap();
    assert_eq!(fresh.subject, cold.subject);
    assert_eq!(fresh.celebrities, cold.celebrities);
    reopened.set_now(NOW + media::DEFAULT_TTL_SECS + 1);
    let stale = reopened.subject_with_credits("1292720").unwrap();
    assert_eq!(stale.subject, cold.subject);
    assert_eq!(stale.celebrities, cold.celebrities);
}
#[test]
fn catalog_not_found_and_transport_failure_remain_distinct() {
    let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let source = Source(Arc::new(Mutex::new(Reply::Missing)));
    let douban = Douban::new_at(source.clone(), &dir.path().join("cache.db"), NOW).unwrap();
    assert!(matches!(
        douban.subject_with_credits("missing").unwrap_err(),
        TmdbError::NotFound(_)
    ));
    *source.0.lock().unwrap() = Reply::Failure;
    assert!(matches!(
        douban.subject_with_credits("unavailable").unwrap_err(),
        TmdbError::Http(_)
    ));
    *source.0.lock().unwrap() = Reply::Present;
    assert_eq!(
        douban.subject_with_credits("unavailable").unwrap().subject["title"],
        "Forrest Gump"
    );
    assert!(
        matches!(
            douban.subject_with_credits("missing").unwrap_err(),
            TmdbError::NotFound(_)
        ),
        "a fresh negative entry must remain explicitly missing"
    );
    douban.set_now(NOW + media::DEFAULT_TTL_SECS + 1);
    assert_eq!(
        douban.subject_with_credits("missing").unwrap().subject["title"],
        "Forrest Gump"
    );
}

#[test]
fn malformed_douban_components_cannot_replace_valid_durable_pairs() {
    for broken in [Reply::MalformedSubject, Reply::MalformedCredits] {
        let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
        let db = dir.path().join("cache.db");
        let source = Source(Arc::new(Mutex::new(Reply::Present)));
        let douban = Douban::new_at(source.clone(), &db, NOW).unwrap();
        let original = douban.subject_with_credits("1292720").unwrap();
        douban.set_now(NOW + media::DEFAULT_TTL_SECS + 1);
        *source.0.lock().unwrap() = broken;
        let stale = douban.subject_with_credits("1292720").unwrap();
        assert_eq!(stale.subject, original.subject);
        assert_eq!(stale.celebrities, original.celebrities);
        drop(douban);
        *source.0.lock().unwrap() = Reply::Failure;
        let reopened = Douban::new_at(source, &db, NOW + media::DEFAULT_TTL_SECS + 2).unwrap();
        let durable = reopened.subject_with_credits("1292720").unwrap();
        assert_eq!(durable.subject, original.subject);
        assert_eq!(durable.celebrities, original.celebrities);
    }
}

struct PairedOrigin {
    entered: Mutex<u8>,
    ready: std::sync::Condvar,
}
impl CatalogGet for PairedOrigin {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        let bit = if path.contains("/celebrities") { 2 } else { 1 };
        let mut entered = self.entered.lock().unwrap();
        *entered |= bit;
        self.ready.notify_all();
        let (entered, _) = self
            .ready
            .wait_timeout_while(entered, std::time::Duration::from_millis(500), |entered| {
                *entered != 3
            })
            .unwrap();
        if *entered != 3 {
            return Err(TmdbError::Http(
                "the independent pair was executed serially".into(),
            ));
        }
        Ok(if bit == 1 { SUBJECT } else { CREDITS }.into())
    }
}
#[test]
fn independent_douban_components_are_requested_together_and_remain_complete() {
    let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let douban = Douban::new_at(
        PairedOrigin {
            entered: Mutex::new(0),
            ready: std::sync::Condvar::new(),
        },
        &dir.path().join("cache.db"),
        NOW,
    )
    .unwrap();
    let pair = douban
        .subject_with_credits("1292720")
        .expect("both finite independent components must complete together");
    assert_eq!(pair.subject["title"], "Forrest Gump");
    assert_eq!(pair.celebrities["actors"][0]["name"], "Tom Hanks");
    assert_eq!(pair.celebrities["directors"][0]["name"], "Robert Zemeckis");
}

#[test]
fn cold_malformed_douban_components_are_errors_and_do_not_poison_recovery() {
    for broken in [Reply::MalformedSubject, Reply::MalformedCredits] {
        let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
        let source = Source(Arc::new(Mutex::new(broken)));
        let db = dir.path().join("cache.db");
        let douban = Douban::new_at(source.clone(), &db, NOW).unwrap();
        let error = douban.subject_with_credits("1292720").unwrap_err();
        assert!(
            !matches!(error, TmdbError::NotFound(_)),
            "a malformed transient envelope is not a genuine missing subject"
        );
        *source.0.lock().unwrap() = Reply::Present;
        let recovered = douban.subject_with_credits("1292720").unwrap();
        assert_eq!(recovered.subject["id"], "1292720");
        assert_eq!(recovered.subject["title"], "Forrest Gump");
        assert_eq!(recovered.celebrities["actors"][0]["name"], "Tom Hanks");
        drop(douban);
        *source.0.lock().unwrap() = Reply::Failure;
        let reopened = Douban::new_at(source, &db, NOW + 1).unwrap();
        let durable = reopened.subject_with_credits("1292720").unwrap();
        assert_eq!(durable.subject, recovered.subject);
        assert_eq!(durable.celebrities, recovered.celebrities);
    }
}
