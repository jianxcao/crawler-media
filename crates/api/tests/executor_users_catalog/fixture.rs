use api::{ApiState, Store};
use domain::{User, UserId, UserRole};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use std::sync::Arc;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}
pub struct Fixture {
    pub _tmp: tempfile::TempDir,
    pub state: ApiState,
    pub member: UserId,
}
impl Fixture {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state = ApiState::new(
            Store::open(tmp.path().join("data")).unwrap(),
            "users-cache-secret".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
            tmp.path().join("library"),
        )
        .unwrap();
        let member = UserId::new();
        {
            let handle = state.store();
            let store = handle.lock();
            store
                .insert_user_with_password(
                    &User {
                        id: member,
                        login: "member".into(),
                        enabled: true,
                        role: UserRole::Member,
                    },
                    "original-password",
                )
                .unwrap();
            store.set_user_token(member, "member-session").unwrap();
            store
                .put_catalog_cache(
                    "tmdb",
                    "/movie/1",
                    r#"{"id":1,"title":"Cached","release_date":"二〇二六"}"#,
                    100,
                    None,
                )
                .unwrap();
        }
        Self {
            _tmp: tmp,
            state,
            member,
        }
    }
}
