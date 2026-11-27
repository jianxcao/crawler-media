#![allow(dead_code)]

#[path = "../plugin_commands/support.rs"]
mod cmd_support;
pub mod helpers;
pub mod pkg;

#[allow(unused_imports)]
pub use helpers::*;

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;

use api::plugins::*;
use api::{ApiState, Store};
use domain::UserId;
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use pkg::TestSigningKey;
use store::plugins::{CanonicalPluginRequest, PluginGrant, PluginOperationState};

#[allow(unused_imports)]
pub use cmd_support::{TOKEN, site, tick};

pub const ADMIN_PASS: &str = "p1c-admin-pass-123!";

struct OfflineFetcher;
impl Fetcher for OfflineFetcher {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

pub fn admin_id() -> UserId {
    UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap()
}

pub fn build_state(dir: &std::path::Path) -> ApiState {
    let store = Store::open(dir.join("data")).unwrap();
    let dl = Arc::new(MemoryDownloader::new(dir.join("stage")));
    let state = ApiState::new_arc_with_admin_password(
        Arc::new(store),
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(OfflineFetcher),
        dl,
    )
    .unwrap();
    {
        let jobs = state.jobs();
        let queue = jobs.acquire().unwrap();
        for def in queue.list_defs().unwrap() {
            if !def.kind.is_plugin() {
                let _ = queue.set_def_enabled_by_id(def.id, false);
            }
        }
    }
    state
}

pub fn req_ctx(
    method: &str,
    path: &str,
    key: &str,
    body_byte: u8,
) -> (String, CanonicalPluginRequest) {
    (
        key.to_string(),
        CanonicalPluginRequest {
            method: method.into(),
            path: path.into(),
            body_digest: [body_byte; 32],
        },
    )
}

pub async fn issue_reauth(state: &ApiState, purpose: ReauthPurpose) -> String {
    state
        .plugins()
        .issue_reauth_token(state, admin_id(), ADMIN_PASS.into(), purpose)
        .await
        .unwrap()
        .reauth_token
}

pub async fn enable_dev_mode(state: &ApiState) {
    let reauth = issue_reauth(state, ReauthPurpose::DeveloperModeEnable).await;
    let (k, r) = req_ctx("POST", "/settings/plugins/developer/enable", "dev-en", 99);
    let settings = state
        .plugins()
        .enable_developer_mode(
            state,
            admin_id(),
            EnableDeveloperModeInput {
                reauth_token: reauth,
                expected_revision: 0,
                idempotency_key: k,
                request: r,
            },
        )
        .await
        .unwrap();
    assert!(settings.developer_mode);
}

pub async fn pin_trust_key(state: &ApiState, key: &TestSigningKey) -> TrustKeyView {
    let actor = admin_id();
    let prep = state
        .plugins()
        .prepare_trust_key(
            state,
            actor,
            PrepareTrustKeyInput {
                public_key: key.public_key_str.clone(),
                label: "CI Key".into(),
            },
        )
        .await
        .unwrap();
    let reauth = issue_reauth(state, ReauthPurpose::TrustKeyCreate).await;
    let (k, r) = req_ctx(
        "POST",
        "/plugin-trust-keys",
        &format!("tk-{}", prep.key_id),
        98,
    );
    state
        .plugins()
        .create_trust_key(
            state,
            actor,
            CreateTrustKeyInput {
                label: Some("CI Key".into()),
                public_key: prep.public_key,
                fingerprint: prep.fingerprint,
                consent_token: prep.consent_token,
                reauth_token: reauth,
                expected_revision: 0,
                idempotency_key: k,
                request: r,
            },
        )
        .await
        .unwrap()
}

pub async fn upload_pkg(
    state: &ApiState,
    pkg_bytes: Vec<u8>,
    sig: Option<String>,
) -> Result<StagedUploadView, PluginServiceError> {
    let upload_id = uuid::Uuid::new_v4();
    let reauth = issue_reauth(state, ReauthPurpose::PluginUpload).await;
    let (k, r) = req_ctx("POST", "/plugin-uploads", &format!("up-{upload_id}"), 97);
    state
        .plugins()
        .stage_upload(
            state,
            StageUploadInput {
                actor_user_id: admin_id(),
                upload_id,
                cmplugin_bytes: pkg_bytes,
                signature_text: sig,
                expected_sha256: None,
                reauth_token: reauth,
                idempotency_key: k,
                request: r,
            },
        )
        .await
}

pub async fn upload_and_install(
    state: &ApiState,
    pkg_bytes: Vec<u8>,
    sig: Option<String>,
    grants: Vec<PluginGrant>,
) -> PluginDetailView {
    let actor = admin_id();
    let upload = upload_pkg(state, pkg_bytes, sig).await.unwrap();
    let (k1, r1) = req_ctx(
        "POST",
        "/plugin-installations/prepare",
        &format!("p-{}", upload.upload_id),
        1,
    );
    let prep_op = state
        .plugins()
        .prepare_install(
            state,
            PrepareInstallInput {
                actor_user_id: actor,
                upload_id: Some(upload.upload_id),
                repo_id: None,
                plugin_id: None,
                version: None,
                idempotency_key: k1,
                request: r1,
            },
        )
        .await
        .unwrap();
    assert_eq!(prep_op.state, PluginOperationState::Succeeded);
    let prep: InstallationPreparationView =
        serde_json::from_value(prep_op.result.unwrap()).unwrap();
    let (k2, r2) = req_ctx(
        "POST",
        "/plugin-installations",
        &format!("i-{}", upload.upload_id),
        2,
    );
    let inst_op = state
        .plugins()
        .execute_install(
            state,
            ExecuteInstallInput {
                actor_user_id: actor,
                preparation_id: prep.preparation_id,
                consent_token: prep.consent_token,
                grants,
                expected_revision: 0,
                idempotency_key: k2,
                request: r2,
            },
        )
        .await
        .unwrap();
    assert_eq!(inst_op.state, PluginOperationState::Succeeded);
    state
        .plugins()
        .get_plugin(state, &prep.plugin_id)
        .await
        .unwrap()
}

pub async fn enable_plugin_ok(
    state: &ApiState,
    plugin_id: &str,
    expected_revision: i64,
    key: &str,
) -> OperationAdminView {
    let (ek, er) = req_ctx("POST", &format!("/plugins/{plugin_id}/enable"), key, 3);
    let op = state
        .plugins()
        .enable_plugin(
            state,
            plugin_id,
            EnablePluginInput {
                actor_user_id: admin_id(),
                expected_revision,
                idempotency_key: ek,
                request: er,
            },
        )
        .await
        .unwrap();
    assert_eq!(op.state, PluginOperationState::Succeeded);
    op
}

pub async fn prepare_upgrade_ok(
    state: &ApiState,
    plugin_id: &str,
    version: &str,
    src: (Option<uuid::Uuid>, Option<String>),
    opts: (&str, &str, i64, &str),
) -> UpgradePreparationView {
    let (upload_id, package_hash) = src;
    let (state_policy, mode, expected_revision, key) = opts;
    let schema = state
        .plugins()
        .get_config_schema(state, plugin_id)
        .await
        .unwrap();
    let (pk, pr) = req_ctx(
        "POST",
        &format!("/plugins/{plugin_id}/upgrades/prepare"),
        key,
        13,
    );
    let op = state
        .plugins()
        .prepare_upgrade(
            state,
            plugin_id,
            PrepareUpgradeInput {
                actor_user_id: admin_id(),
                version: version.into(),
                upload_id,
                package_hash,
                repo_id: None,
                target_config: UpgradeTargetConfigInput {
                    schema_revision: schema.schema_revision,
                    values: serde_json::json!({"mode": mode}),
                    secrets: BTreeMap::new(),
                },
                site_id: None,
                state_policy: state_policy.into(),
                reset_scopes: None,
                expected_revision,
                idempotency_key: pk,
                request: pr,
            },
        )
        .await
        .unwrap();
    serde_json::from_value(op.result.unwrap()).unwrap()
}

pub fn plant_unknown_schedule_cmd(
    state: &ApiState,
    plugin_id: &str,
    actor: UserId,
    idem_key: &str,
) -> uuid::Uuid {
    let p_en = state.store().get_plugin(plugin_id).unwrap().unwrap();
    let auth = state
        .store()
        .issue_plugin_state_authority(&p_en, None)
        .unwrap();
    let cmd_auth = state
        .store()
        .authorize_plugin_command(
            &auth,
            "schedule.set",
            &[store::plugins::PluginCommandTarget::Global],
        )
        .unwrap();
    let (ck, cr) = req_ctx(
        "POST",
        &format!("/plugins/{plugin_id}/commands"),
        idem_key,
        50,
    );
    let unk = state
        .store()
        .accept_plugin_command(
            &cmd_auth,
            &store::plugins::AcceptPluginOperation {
                plugin_id: Some(plugin_id.into()),
                subject: store::plugins::PluginSubject::User(actor),
                idempotency_key: ck,
                request: cr,
                kind: "schedule.set".into(),
                expected_revision: Some(p_en.revision),
                expected_generation: Some(p_en.generation),
                cause: store::plugins::PluginCommandCause::root(idem_key),
            },
            b"{}",
        )
        .unwrap();
    let claim = state
        .store()
        .claim_plugin_command(
            unk.operation.operation_id,
            unk.operation.revision,
            Some(p_en.generation),
            0,
            10,
        )
        .unwrap();
    state
        .store()
        .record_plugin_command_interruption(
            claim.operation_id(),
            claim.revision(),
            claim.generation(),
            claim.attempt(),
        )
        .unwrap();
    unk.operation.operation_id
}

pub fn reconcile_schedule_cmd(state: &ApiState, op_id: uuid::Uuid) {
    let admin = state
        .store()
        .get_plugin_operation_admin(op_id)
        .unwrap()
        .unwrap();
    state
        .store()
        .reconcile_plugin_command(&store::plugins::PluginReconciliationObservation {
            operation_id: op_id,
            revision: admin.operation.revision,
            generation: admin.operation.generation,
            attempt: admin.attempt,
            outcome: store::plugins::PluginReconciliationOutcome::ConfirmedNoEffect,
        })
        .unwrap();
}

pub async fn exec_upgrade_req(
    state: &ApiState,
    pid: &str,
    prep: UpgradePreparationView,
    grants: Vec<PluginGrant>,
    opts: (&str, Option<String>, i64, &str),
) -> Result<OperationAdminView, PluginServiceError> {
    let (state_policy, reauth, rev, key) = opts;
    let (uk, ur) = req_ctx("POST", &format!("/plugins/{pid}/upgrade"), key, 15);
    state
        .plugins()
        .execute_upgrade(
            state,
            pid,
            ExecuteUpgradeInput {
                actor_user_id: admin_id(),
                preparation_id: prep.preparation_id,
                consent_token: prep.consent_token,
                grants,
                state_policy: state_policy.into(),
                target_config_digest: prep.config_diff.target_config_digest,
                reauth_token: reauth,
                expected_revision: rev,
                idempotency_key: uk,
                request: ur,
            },
        )
        .await
}

pub async fn exec_rollback_req(
    state: &ApiState,
    pid: &str,
    package_hash: &str,
    prep: UpgradePreparationView,
    grants: Vec<PluginGrant>,
    opts: (&str, Option<String>, i64, &str),
) -> Result<OperationAdminView, PluginServiceError> {
    let (state_policy, reauth, rev, key) = opts;
    let (rk, rr) = req_ctx("POST", &format!("/plugins/{pid}/rollback"), key, 16);
    state
        .plugins()
        .rollback_plugin(
            state,
            pid,
            RollbackInput {
                actor_user_id: admin_id(),
                package_hash: package_hash.to_string(),
                preparation_id: prep.preparation_id,
                consent_token: prep.consent_token,
                grants,
                state_policy: state_policy.into(),
                target_config_digest: prep.config_diff.target_config_digest,
                reauth_token: reauth,
                expected_revision: rev,
                idempotency_key: rk,
                request: rr,
            },
        )
        .await
}

pub async fn run_task_req(
    state: &ApiState,
    pid: &str,
    input: serde_json::Value,
    expected_revision: i64,
    key: &str,
) -> Result<OperationAdminView, PluginServiceError> {
    let (rk, rr) = req_ctx("POST", &format!("/plugins/{pid}/tasks/sync/run"), key, 62);
    state
        .plugins()
        .run_task(
            state,
            pid,
            "sync",
            RunTaskInput {
                actor_user_id: admin_id(),
                input,
                expected_revision,
                idempotency_key: rk,
                request: rr,
            },
        )
        .await
}

pub async fn update_task_req(
    state: &ApiState,
    pid: &str,
    config: serde_json::Value,
    expected_revision: i64,
    key: &str,
) -> Result<OperationAdminView, PluginServiceError> {
    let (uk, ur) = req_ctx("PUT", &format!("/plugins/{pid}/tasks/sync"), key, 81);
    state
        .plugins()
        .update_task(
            state,
            pid,
            "sync",
            UpdateTaskInput {
                actor_user_id: admin_id(),
                enabled: true,
                interval_secs: 120,
                config,
                expected_revision,
                idempotency_key: uk,
                request: ur,
            },
        )
        .await
}

pub async fn grant_kv_req(
    state: &ApiState,
    pid: &str,
    expected_revision: i64,
    key: &str,
) -> Result<OperationAdminView, PluginServiceError> {
    let actor = admin_id();
    let kv_grant = PluginGrant {
        capability: "kv".into(),
        scope: store::plugins::PluginGrantScope::Global,
        sensitive: false,
    };
    let prep = state
        .plugins()
        .prepare_permissions(
            state,
            actor,
            pid,
            PreparePermissionsInput {
                requested_grants: vec![kv_grant.clone()],
            },
        )
        .await
        .unwrap();
    let (gk, gr) = req_ctx(
        "POST",
        &format!("/plugins/{pid}/permissions/grant"),
        key,
        83,
    );
    state
        .plugins()
        .grant_permissions(
            state,
            pid,
            GrantPermissionsInput {
                actor_user_id: actor,
                consent_token: prep.consent_token,
                grants: vec![kv_grant],
                expected_revision,
                idempotency_key: gk,
                request: gr,
            },
        )
        .await
}

pub async fn disable_plugin_ok(
    state: &ApiState,
    pid: &str,
    expected_revision: i64,
    key: &str,
) -> PluginDetailView {
    let (dk, dr) = req_ctx("POST", &format!("/plugins/{pid}/disable"), key, 6);
    let op = state
        .plugins()
        .disable_plugin(
            state,
            pid,
            DisablePluginInput {
                actor_user_id: admin_id(),
                reason: None,
                expected_revision,
                idempotency_key: dk,
                request: dr,
            },
        )
        .await
        .unwrap();
    assert_eq!(op.state, PluginOperationState::Succeeded);
    state.plugins().get_plugin(state, pid).await.unwrap()
}

pub async fn exec_state_reset_req(
    state: &ApiState,
    pid: &str,
    consent_token: String,
    reauth_token: String,
    expected_revision: i64,
    key: &str,
) -> Result<OperationAdminView, PluginServiceError> {
    let (sk, sr) = req_ctx("POST", &format!("/plugins/{pid}/state/reset"), key, 7);
    state
        .plugins()
        .execute_state_reset(
            state,
            pid,
            ExecuteStateResetInput {
                actor_user_id: admin_id(),
                security_scopes: vec![],
                consent_token,
                reauth_token,
                expected_revision,
                idempotency_key: sk,
                request: sr,
            },
        )
        .await
}

pub async fn revoke_trust_key_req(
    state: &ApiState,
    key_id: &str,
    reason: &str,
    reauth_token: String,
    expected_revision: i64,
    key: &str,
) -> Result<OperationAdminView, PluginServiceError> {
    let (rk, rr) = req_ctx("POST", "/plugin-trust-keys/revoke", key, 94);
    state
        .plugins()
        .revoke_trust_key(
            state,
            admin_id(),
            RevokeTrustKeyInput {
                key_id: key_id.into(),
                reason: reason.into(),
                reauth_token,
                expected_revision,
                idempotency_key: rk,
                request: rr,
            },
        )
        .await
}

pub async fn install_from_upload(
    state: &ApiState,
    upload_id: uuid::Uuid,
    grants: Vec<PluginGrant>,
    key: &str,
) -> Result<OperationAdminView, PluginServiceError> {
    let actor = admin_id();
    let (pk, pr) = req_ctx(
        "POST",
        "/plugin-installations/prepare",
        &format!("{key}-p"),
        98,
    );
    let prep_op = state
        .plugins()
        .prepare_install(
            state,
            PrepareInstallInput {
                actor_user_id: actor,
                upload_id: Some(upload_id),
                repo_id: None,
                plugin_id: None,
                version: None,
                idempotency_key: pk,
                request: pr,
            },
        )
        .await?;
    let prep: InstallationPreparationView =
        serde_json::from_value(prep_op.result.unwrap()).unwrap();
    let (ik, ir) = req_ctx("POST", "/plugin-installations", &format!("{key}-i"), 99);
    state
        .plugins()
        .execute_install(
            state,
            ExecuteInstallInput {
                actor_user_id: actor,
                preparation_id: prep.preparation_id,
                consent_token: prep.consent_token,
                grants,
                expected_revision: 0,
                idempotency_key: ik,
                request: ir,
            },
        )
        .await
}
