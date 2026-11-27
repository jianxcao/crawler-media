#[path = "plugin_commands/support.rs"]
mod support;

use std::sync::Arc;

use api::ApiState;
use api::plugins::reconcile_pending;
use plugin_api::{EndpointRequest, EndpointResponse, ErrorCode, Failure, Header};
use store::ManualCredentialWrite;
use store::plugins::*;
use support::*;

fn register_plugin(
    state: &ApiState,
    id: &str,
    rel: &str,
    wasm: &[u8],
    credential_bound: bool,
) -> PluginRecord {
    let path = state.store().data_dir().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, wasm).unwrap();
    let meta = PluginVersionMetadata {
        plugin_id: id.into(),
        package_hash: format!("{id}-hash-1"),
        version: "1.0.0".into(),
        manifest_json: "{}".into(),
        config_schema_json: "{}".into(),
        state_schema: "1".into(),
        artifact_path: rel.into(),
        signing_key_id: None,
        signature_status: "development".into(),
        ui_metadata_ref: None,
        credential_bound,
    };
    state.store().register_plugin_version(&meta, 0).unwrap()
}

fn grant_and_enable(state: &ApiState, p: &PluginRecord, grants: &[PluginGrant]) -> PluginRecord {
    let granted = state
        .store()
        .replace_plugin_grants(&p.plugin_id, &p.package_hash, p.revision, grants)
        .unwrap();
    enable(state.store().as_ref(), &granted)
}

struct EpCtx<'a> {
    state: &'a ApiState,
    plugin: &'a PluginRecord,
    subject: &'a PluginSubject,
}

fn ep_call(
    ctx: &EpCtx<'_>,
    auth: PluginStateAuthority,
    cause_id: &str,
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<EndpointResponse, Failure> {
    ctx.state.plugins().invoke_endpoint(
        ctx.state,
        ctx.plugin,
        auth,
        ctx.subject.clone(),
        PluginCommandCause::root(cause_id),
        EndpointRequest {
            method: method.into(),
            path: path.into(),
            query: Vec::<Header>::new(),
            body: body.to_vec(),
        },
    )
}

fn read_state(
    state: &ApiState,
    auth: &PluginStateAuthority,
    kind: PluginStateKind,
    key: &str,
) -> Option<Vec<u8>> {
    state
        .store()
        .plugin_state_get(auth, kind, key)
        .unwrap()
        .map(|entry| entry.value.as_bytes().to_vec())
}

#[test]
fn wasm_task_executes_via_jobs_tick_isolates_kv_blob_and_submits_command() {
    let tmp = tempfile::tempdir().unwrap();
    let mut effects = FakeEffects::new(FakeEffectMode::Succeed);
    effects.supported_kinds.push("site.check-in".into());
    let effects = Arc::new(effects);
    let state = build_state(tmp.path(), effects.clone(), None);

    let installed = register_plugin(
        &state,
        "org.example.task",
        "plugins/pkg/org.example.task/v1/component.wasm",
        include_bytes!("../../plugin-host/tests/fixtures/task_fixture.wasm"),
        false,
    );
    let configured = state
        .store()
        .save_plugin_config(
            "org.example.task",
            installed.revision,
            "1",
            &serde_json::json!({"mode": "wasm"}),
        )
        .unwrap();
    let grants = ["kv", "blob", "command:site.check-in"]
        .into_iter()
        .map(|cap| PluginGrant {
            capability: cap.into(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        })
        .collect::<Vec<_>>();
    let enabled = grant_and_enable(&state, &configured, &grants);
    state
        .store()
        .save_plugin_task_binding(
            &enabled,
            "sync",
            0,
            &PluginTaskDefinition {
                enabled: true,
                interval_secs: Some(60),
                public_config: serde_json::json!({"action": "submit-and-state"}),
            },
        )
        .unwrap();

    reconcile_pending(&state, 5).unwrap();
    tick(&state, 10);
    tick(&state, 15);

    let auth = state
        .store()
        .issue_plugin_state_authority(&enabled, None)
        .unwrap();
    assert_eq!(
        read_state(&state, &auth, PluginStateKind::Kv, "k/a").as_deref(),
        Some(b"val-a".as_slice())
    );
    assert!(read_state(&state, &auth, PluginStateKind::Kv, "k/tmp").is_none());
    assert_eq!(
        read_state(&state, &auth, PluginStateKind::Blob, "b/a").as_deref(),
        Some(b"blob-val".as_slice())
    );
    assert!(read_state(&state, &auth, PluginStateKind::Blob, "b/tmp").is_none());
    assert_eq!(effects.calls(), 1);

    let subject = PluginSubject::Task {
        plugin_id: "org.example.task".into(),
        task_id: "sync".into(),
    };
    let ops = state
        .store()
        .list_plugin_operations(&subject, None, None)
        .unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].state, PluginOperationState::Succeeded);
}

fn setup_two_site_ep_plugin(state: &ApiState) -> (domain::Site, domain::Site, PluginRecord) {
    let (site_a, mut site_b) = (site(), site());
    site_b.name = "Site B".into();
    state.store().insert_site(&site_a).unwrap();
    state.store().insert_site(&site_b).unwrap();
    let installed = register_plugin(
        state,
        "org.example.ep",
        "plugins/pkg/org.example.ep/v1/component.wasm",
        include_bytes!("../../plugin-host/tests/fixtures/endpoint_fixture.wasm"),
        true,
    );
    let grants: Vec<_> = [
        ("kv", site_a.id),
        ("command:site.verify-session", site_a.id),
        ("kv", site_b.id),
    ]
    .into_iter()
    .map(|(cap, sid)| PluginGrant {
        capability: cap.into(),
        scope: PluginGrantScope::Site(sid),
        sensitive: false,
    })
    .collect();
    let enabled = grant_and_enable(state, &installed, &grants);
    (site_a, site_b, enabled)
}

#[test]
fn wasm_endpoint_shares_credential_domain_and_isolates_across_site_epoch() {
    let tmp = tempfile::tempdir().unwrap();
    let mut effects = FakeEffects::new(FakeEffectMode::Succeed);
    effects.supported_kinds.push("site.verify-session".into());
    let effects = Arc::new(effects);
    let state = build_state(tmp.path(), effects.clone(), None);
    let (site_a, site_b, enabled) = setup_two_site_ep_plugin(&state);

    let obs_a1 = state.store().observe_site(site_a.id).unwrap().unwrap();
    let obs_b = state.store().observe_site(site_b.id).unwrap().unwrap();
    let auth_a1 = state
        .store()
        .issue_plugin_state_authority(&enabled, Some(&obs_a1))
        .unwrap();
    let auth_b = state
        .store()
        .issue_plugin_state_authority(&enabled, Some(&obs_b))
        .unwrap();
    let subject = PluginSubject::User(domain::UserId::new());
    let ctx = EpCtx {
        state: &state,
        plugin: &enabled,
        subject: &subject,
    };

    assert_eq!(
        ep_call(
            &ctx,
            auth_a1.clone(),
            "ep-put",
            "POST",
            "/kv-put",
            b"secret-a"
        )
        .unwrap()
        .status,
        200
    );
    let get_a = ep_call(&ctx, auth_a1.clone(), "ep-ga", "GET", "/kv-get", b"").unwrap();
    assert_eq!(
        (get_a.status, get_a.body.as_slice()),
        (200, b"secret-a".as_slice())
    );
    assert_eq!(
        ep_call(&ctx, auth_b, "ep-gb", "GET", "/kv-get", b"")
            .unwrap()
            .status,
        404
    );

    let accepted = ep_call(
        &ctx,
        auth_a1.clone(),
        "ep-sub",
        "POST",
        "/submit",
        b"ep-cmd-1",
    )
    .unwrap();
    assert_eq!(accepted.status, 202);
    let op_id = uuid::Uuid::parse_str(std::str::from_utf8(&accepted.body).unwrap()).unwrap();
    assert_eq!(effects.calls(), 0);
    tick(&state, 10);
    assert_eq!(effects.calls(), 1);
    assert_eq!(
        op_state(&state, &subject, op_id).state,
        PluginOperationState::Succeeded
    );

    state
        .store()
        .save_site(&site_a, ManualCredentialWrite::Replace)
        .unwrap();
    let stale_err = ep_call(&ctx, auth_a1, "ep-st", "GET", "/kv-get", b"").unwrap_err();
    assert!(matches!(stale_err, Failure::Guest(ref err) if err.code == ErrorCode::StaleGeneration));
    let obs_a2 = state.store().observe_site(site_a.id).unwrap().unwrap();
    let auth_a2 = state
        .store()
        .issue_plugin_state_authority(&enabled, Some(&obs_a2))
        .unwrap();
    assert_eq!(
        ep_call(&ctx, auth_a2, "ep-f", "GET", "/kv-get", b"")
            .unwrap()
            .status,
        404
    );
}
