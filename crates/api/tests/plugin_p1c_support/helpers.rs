use api::ApiState;
use api::plugins::*;
use store::plugins::{PluginGrant, PluginOperationState};

use super::{admin_id, req_ctx};

pub async fn prepare_state_reset_ok(state: &ApiState, pid: &str) -> StateResetPreparationView {
    state
        .plugins()
        .prepare_state_reset(
            state,
            admin_id(),
            pid,
            PrepareStateResetInput {
                security_scopes: vec![],
            },
        )
        .await
        .unwrap()
}

pub async fn prepare_permissions_ok(
    state: &ApiState,
    pid: &str,
    grants: Vec<PluginGrant>,
) -> PermissionsPreparationView {
    state
        .plugins()
        .prepare_permissions(
            state,
            admin_id(),
            pid,
            PreparePermissionsInput {
                requested_grants: grants,
            },
        )
        .await
        .unwrap()
}

pub async fn grant_permissions_req(
    state: &ApiState,
    pid: &str,
    consent_token: String,
    grants: Vec<PluginGrant>,
    expected_revision: i64,
    key: &str,
) -> Result<OperationAdminView, PluginServiceError> {
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
                actor_user_id: admin_id(),
                consent_token,
                grants,
                expected_revision,
                idempotency_key: gk,
                request: gr,
            },
        )
        .await
}

pub async fn uninstall_plugin_ok(
    state: &ApiState,
    pid: &str,
    expected_revision: i64,
    key: &str,
) -> OperationAdminView {
    let (uk, ur) = req_ctx("POST", &format!("/plugins/{pid}/uninstall"), key, 84);
    let op = state
        .plugins()
        .uninstall_plugin(
            state,
            pid,
            UninstallPluginInput {
                actor_user_id: admin_id(),
                remove_private_data: false,
                expected_revision,
                idempotency_key: uk,
                request: ur,
            },
        )
        .await
        .unwrap();
    assert_eq!(op.state, PluginOperationState::Succeeded);
    op
}

pub fn plant_disabled_guard_op(state: &ApiState, pid: &str, rev: i64, key: &str) -> uuid::Uuid {
    let (k, r) = req_ctx("POST", &format!("/plugins/{pid}/commands"), key, 9);
    let acc = state
        .store()
        .accept_plugin_operation(&store::plugins::AcceptPluginOperation {
            plugin_id: Some(pid.into()),
            subject: store::plugins::PluginSubject::User(admin_id()),
            idempotency_key: k,
            request: r,
            kind: "disable".into(),
            expected_revision: Some(rev),
            expected_generation: None,
            cause: store::plugins::PluginCommandCause::root(key),
        })
        .unwrap();
    let claim = state
        .store()
        .claim_plugin_command(
            acc.operation.operation_id,
            acc.operation.revision,
            None,
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
    acc.operation.operation_id
}

pub async fn create_trust_key_req(
    state: &ApiState,
    prep: &TrustKeyPreparationView,
    consent_token: String,
    reauth_token: String,
    key: &str,
) -> Result<TrustKeyView, PluginServiceError> {
    let (k, r) = req_ctx("POST", "/plugin-trust-keys", key, 1);
    state
        .plugins()
        .create_trust_key(
            state,
            admin_id(),
            CreateTrustKeyInput {
                label: Some("Peek Key".into()),
                public_key: prep.public_key.clone(),
                fingerprint: prep.fingerprint.clone(),
                consent_token,
                reauth_token,
                expected_revision: 0,
                idempotency_key: k,
                request: r,
            },
        )
        .await
}

pub async fn stage_upload_req(
    state: &ApiState,
    bytes: Vec<u8>,
    reauth_token: String,
    key: &str,
) -> Result<StagedUploadView, PluginServiceError> {
    let (uk, ur) = req_ctx("POST", "/plugin-uploads", key, 3);
    state
        .plugins()
        .stage_upload(
            state,
            StageUploadInput {
                actor_user_id: admin_id(),
                upload_id: uuid::Uuid::new_v4(),
                cmplugin_bytes: bytes,
                signature_text: None,
                expected_sha256: None,
                reauth_token,
                idempotency_key: uk,
                request: ur,
            },
        )
        .await
}
