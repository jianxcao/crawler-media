use super::*;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};

struct RssHook {
    inner: Fixtures,
    deleted: Mutex<Option<(Arc<api::Store>, SubscribeId)>>,
}
impl Fetcher for RssHook {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        if request.key.starts_with("rss:") {
            let deleted = self.deleted.lock().take();
            if let Some((store, id)) = deleted {
                store.delete_subscribe(id).unwrap();
            }
        }
        self.inner.fetch(request)
    }
}

#[tokio::test]
async fn global_rss_arguments_cannot_select_a_subscribe_or_delete_global_identity() {
    assert_global_rss_identity(false, true).await;
}

#[tokio::test]
async fn deleting_a_subscribe_during_global_rss_batch_preserves_global_definition() {
    assert_global_rss_identity(true, false).await;
}

async fn assert_global_rss_identity(delete_during_fetch: bool, extra_argument: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(RssHook {
        inner: Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::from([("rss", rss_xml())]),
        },
        deleted: Mutex::new(None),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let state = api::ApiState::new(
        api::Store::open(tmp.path().join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        fetcher.clone(),
        downloader.clone(),
    )
    .unwrap();
    let app = router(state.clone());
    create_site(&app).await;
    let ids = create_rss_subscribes(&app).await;
    let rss = {
        let jobs = state.jobs();
        let q = jobs.acquire().unwrap();
        q.with_connection(|c| {
            c.execute(
                "UPDATE job_defs SET enabled=0 WHERE kind<>'subscribe_rss'",
                [],
            )
        })
        .unwrap();
        let rss = q
            .def_for_identity(jobs::JobIdentity::global(jobs::JobKind::SubscribeRss))
            .unwrap()
            .unwrap();
        if extra_argument {
            let args =
                json!({"subscribe_id":SubscribeId::new(),"note":"arguments only"}).to_string();
            q.set_def_payload_cas(rss.id, &rss.payload, &args).unwrap();
        }
        rss
    };
    if delete_during_fetch {
        *fetcher.deleted.lock() = Some((state.store(), ids[0]));
    }
    let response = app
        .oneshot(request(
            "POST",
            "/api/v1/jobs/tick?now=1",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        state
            .jobs()
            .acquire()
            .unwrap()
            .def(rss.id)
            .unwrap()
            .is_some(),
        "global RSS identity must survive unrelated Subscribe disappearance/arguments"
    );
    assert_eq!(state.store().load_pending(ids[1]).unwrap().len(), 1);
    if !delete_during_fetch {
        assert_eq!(state.store().load_pending(ids[0]).unwrap().len(), 1);
    }
    assert_eq!(
        downloader.added().len(),
        if delete_during_fetch { 1 } else { 2 }
    );
}

#[tokio::test]
async fn orphan_search_disables_definition_without_erasing_its_completed_attempt() {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    );
    let subscribe_id = SubscribeId::new();
    let identity = jobs::JobIdentity::subscribe_search(subscribe_id);
    let (def, attempt) = {
        let jobs = state.jobs();
        let q = jobs.acquire().unwrap();
        for def in q.list_defs().unwrap() {
            q.set_def_enabled_by_id(def.id, false).unwrap();
        }
        let def = q
            .create_def(jobs::NewDef {
                kind: identity.kind,
                scope: identity.scope,
                name: "orphan Search".into(),
                enabled: true,
                schedule: Some(jobs::Schedule::Interval { secs: 300 }),
                payload: json!({"subscribe_id":subscribe_id}).to_string(),
                timeout_secs: None,
                concurrency_key: Some("orphan-search".into()),
            })
            .unwrap();
        let attempt = q.ensure_scheduled_for(0, def.id).unwrap().remove(0);
        (def, attempt)
    };
    let app = router(state.clone());
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/jobs/tick?now=1",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_data(response).await["ran"], 1);
    {
        let jobs = state.jobs();
        let q = jobs.acquire().unwrap();
        let completed = q.get(attempt.id).unwrap().unwrap();
        assert_eq!(completed.def_id, Some(def.id));
        assert_eq!(completed.status, jobs::JobStatus::Succeeded);
        assert_eq!(completed.finished_at, Some(1));
        assert!(!q.def(def.id).unwrap().unwrap().enabled);
        assert!(q.get_live_child(def.id).unwrap().is_none());
    }
    let response = app
        .oneshot(request(
            "POST",
            "/api/v1/jobs/tick?now=10000",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(json_data(response).await["ran"], 0);
    let jobs = state.jobs();
    let q = jobs.acquire().unwrap();
    let history = q.jobs_for_identity(identity, 10).unwrap();
    assert_eq!(history.len(), 1);
    assert!(history.iter().any(|job| job.id == attempt.id
        && job.status == jobs::JobStatus::Succeeded
        && job.finished_at == Some(1)));
}
