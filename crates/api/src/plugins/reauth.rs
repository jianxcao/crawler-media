use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use domain::{UserId, UserRole};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use store::plugins::{PluginGrant, PluginSecurityScope};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use super::types::{PluginServiceError, ReauthPurpose, ReauthTokenView, format_rfc3339};
use crate::http::store_wait::store_read;
use crate::management::ApiState;

const REAUTH_TTL_SECS: i64 = 300;
const CONSENT_TTL_SECS: i64 = 900;
const MAX_TOKENS: usize = 2048;

pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Mixes three OS-CSPRNG UUIDv4 samples (`3 * 122 = 366` bits of entropy) via SHA-256
/// to produce 256 uniform random bits with zero fixed version/variant nibbles.
pub(super) fn random_256bit_entropy() -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(Uuid::new_v4().as_bytes());
    hasher.update(Uuid::new_v4().as_bytes());
    hasher.update(Uuid::new_v4().as_bytes());
    hasher.finalize().into()
}

fn mint_random_token() -> (String, [u8; 32]) {
    let raw = random_256bit_entropy();
    let mut hex = String::with_capacity(64);
    for byte in raw {
        hex.push_str(&format!("{byte:02x}"));
    }
    let digest: [u8; 32] = Sha256::digest(hex.as_bytes()).into();
    (hex, digest)
}

fn sha256_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

struct ReauthRecord {
    user_id: UserId,
    purpose: ReauthPurpose,
    expires_at: i64,
}

pub(crate) struct ReauthService {
    permits: Arc<Semaphore>,
    tokens: Mutex<HashMap<[u8; 32], ReauthRecord>>,
}

impl Default for ReauthService {
    fn default() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(2)),
            tokens: Mutex::new(HashMap::new()),
        }
    }
}

impl ReauthService {
    pub(crate) async fn issue_reauth_token(
        &self,
        state: &ApiState,
        user_id: UserId,
        password: String,
        purpose: ReauthPurpose,
    ) -> Result<ReauthTokenView, PluginServiceError> {
        if password.is_empty() {
            return Err(PluginServiceError::invalid(
                "auth.invalid",
                "password must not be empty",
            ));
        }
        let user = store_read(state, &[store::Db::App], move |store| {
            store.get_user(user_id)
        })
        .await
        .map_err(|e| PluginServiceError::Busy {
            code: "runtime.busy",
            message: e.to_string(),
        })??
        .ok_or_else(|| PluginServiceError::Unauthenticated {
            code: "auth.invalid",
            message: "user not found".into(),
        })?;
        if !user.enabled {
            return Err(PluginServiceError::Unauthenticated {
                code: "auth.invalid",
                message: "user disabled".into(),
            });
        }
        if user.role != UserRole::Admin {
            return Err(PluginServiceError::forbidden(
                "auth.forbidden",
                "admin role required",
            ));
        }

        let permit: OwnedSemaphorePermit =
            self.permits
                .clone()
                .try_acquire_owned()
                .map_err(|_| PluginServiceError::Busy {
                    code: "runtime.busy",
                    message: "password verification busy".into(),
                })?;

        let login = user.login.clone();
        let cred = store_read(state, &[store::Db::App], move |store| {
            store.login_credential(&login)
        })
        .await
        .map_err(|e| PluginServiceError::Busy {
            code: "runtime.busy",
            message: e.to_string(),
        })??
        .ok_or_else(|| PluginServiceError::Unauthenticated {
            code: "auth.invalid",
            message: "invalid credentials".into(),
        })?;

        let verified = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            store::password::verify_password(&password, &cred.observed_password)
        })
        .await
        .map_err(|e| PluginServiceError::Io(e.to_string()))?;

        if !verified {
            return Err(PluginServiceError::Unauthenticated {
                code: "auth.invalid",
                message: "invalid credentials".into(),
            });
        }

        let now = now_unix();
        let expires_at = now.saturating_add(REAUTH_TTL_SECS);
        let (raw_token, digest) = mint_random_token();
        let mut guard = self.tokens.lock();
        guard.retain(|_, rec| rec.expires_at >= now);
        if guard.len() >= MAX_TOKENS
            && let Some(oldest) = guard
                .iter()
                .min_by_key(|(_, rec)| rec.expires_at)
                .map(|(k, _)| *k)
        {
            guard.remove(&oldest);
        }
        guard.insert(
            digest,
            ReauthRecord {
                user_id,
                purpose,
                expires_at,
            },
        );
        Ok(ReauthTokenView {
            reauth_token: raw_token,
            expires_at: format_rfc3339(expires_at),
            purpose,
        })
    }

    pub(crate) async fn peek(
        &self,
        state: &ApiState,
        user_id: UserId,
        purpose: ReauthPurpose,
        token: &str,
    ) -> Result<(), PluginServiceError> {
        if token.trim().is_empty() {
            return Err(PluginServiceError::forbidden(
                "plugin.reauth_required",
                "reauth_token required",
            ));
        }
        let user = store_read(state, &[store::Db::App], move |store| {
            store.get_user(user_id)
        })
        .await
        .map_err(|e| PluginServiceError::Busy {
            code: "runtime.busy",
            message: e.to_string(),
        })??
        .ok_or_else(|| PluginServiceError::Unauthenticated {
            code: "auth.invalid",
            message: "user not found".into(),
        })?;
        if !user.enabled || user.role != UserRole::Admin {
            return Err(PluginServiceError::forbidden(
                "auth.forbidden",
                "active admin required",
            ));
        }

        let now = now_unix();
        let digest = sha256_token(token);
        let mut guard = self.tokens.lock();
        guard.retain(|_, rec| rec.expires_at >= now);
        let Some(rec) = guard.get(&digest) else {
            return Err(PluginServiceError::forbidden(
                "plugin.reauth_invalid",
                "invalid, expired, or already consumed reauth_token",
            ));
        };
        if rec.user_id != user_id || rec.purpose != purpose || rec.expires_at < now {
            return Err(PluginServiceError::forbidden(
                "plugin.reauth_invalid",
                "reauth_token does not match caller or purpose",
            ));
        }
        Ok(())
    }

    pub(crate) async fn verify_and_consume(
        &self,
        state: &ApiState,
        user_id: UserId,
        purpose: ReauthPurpose,
        token: &str,
    ) -> Result<(), PluginServiceError> {
        if token.trim().is_empty() {
            return Err(PluginServiceError::forbidden(
                "plugin.reauth_required",
                "reauth_token required",
            ));
        }
        let user = store_read(state, &[store::Db::App], move |store| {
            store.get_user(user_id)
        })
        .await
        .map_err(|e| PluginServiceError::Busy {
            code: "runtime.busy",
            message: e.to_string(),
        })??
        .ok_or_else(|| PluginServiceError::Unauthenticated {
            code: "auth.invalid",
            message: "user not found".into(),
        })?;
        if !user.enabled || user.role != UserRole::Admin {
            return Err(PluginServiceError::forbidden(
                "auth.forbidden",
                "active admin required",
            ));
        }

        let now = now_unix();
        let digest = sha256_token(token);
        let mut guard = self.tokens.lock();
        guard.retain(|_, rec| rec.expires_at >= now);
        let Some(rec) = guard.get(&digest) else {
            return Err(PluginServiceError::forbidden(
                "plugin.reauth_invalid",
                "invalid, expired, or already consumed reauth_token",
            ));
        };
        if rec.user_id != user_id || rec.purpose != purpose || rec.expires_at < now {
            return Err(PluginServiceError::forbidden(
                "plugin.reauth_invalid",
                "reauth_token does not match caller or purpose",
            ));
        }
        guard.remove(&digest);
        Ok(())
    }

    pub(crate) fn peek_in_app_tx(
        &self,
        app: &store::App<'_>,
        user_id: UserId,
        purpose: ReauthPurpose,
        token: &str,
    ) -> Result<(), PluginServiceError> {
        self.check_in_app_tx(app, user_id, purpose, token, false)
    }

    pub(crate) fn consume_in_app_tx(
        &self,
        app: &store::App<'_>,
        user_id: UserId,
        purpose: ReauthPurpose,
        token: &str,
    ) -> Result<(), PluginServiceError> {
        self.check_in_app_tx(app, user_id, purpose, token, true)
    }

    fn check_in_app_tx(
        &self,
        app: &store::App<'_>,
        user_id: UserId,
        purpose: ReauthPurpose,
        token: &str,
        consume: bool,
    ) -> Result<(), PluginServiceError> {
        if token.trim().is_empty() {
            return Err(PluginServiceError::forbidden(
                "plugin.reauth_required",
                "reauth_token required",
            ));
        }
        let user = app
            .get_user(user_id)?
            .ok_or_else(|| PluginServiceError::Unauthenticated {
                code: "auth.invalid",
                message: "user not found".into(),
            })?;
        if !user.enabled || user.role != UserRole::Admin {
            return Err(PluginServiceError::forbidden(
                "auth.forbidden",
                "active admin required",
            ));
        }
        let now = now_unix();
        let digest = sha256_token(token);
        let mut guard = self.tokens.lock();
        guard.retain(|_, rec| rec.expires_at >= now);
        let Some(rec) = guard.get(&digest) else {
            return Err(PluginServiceError::forbidden(
                "plugin.reauth_invalid",
                "invalid, expired, or already consumed reauth_token",
            ));
        };
        if rec.user_id != user_id || rec.purpose != purpose || rec.expires_at < now {
            return Err(PluginServiceError::forbidden(
                "plugin.reauth_invalid",
                "reauth_token does not match caller or purpose",
            ));
        }
        if consume {
            guard.remove(&digest);
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) enum ConsentPayload {
    TrustKey {
        key_id: String,
        fingerprint: String,
        public_key: String,
        label: String,
    },
    Permissions {
        plugin_id: String,
        package_hash: String,
        base_revision: i64,
        grants_digest: [u8; 32],
    },
    Install {
        plugin_id: String,
        version: String,
        package_hash: String,
        upload_id: Uuid,
        allowed_capabilities: Vec<String>,
    },
    Upgrade {
        plugin_id: String,
        version: String,
        package_hash: String,
        upload_id: Option<Uuid>,
        base_revision: i64,
        pre_drained_to_revision: Option<i64>,
        state_policy: String,
        target_config_digest: String,
        target_config: Option<store::plugins::PluginUpgradeConfig>,
        reset_scopes: Option<Vec<PluginSecurityScope>>,
        allowed_capabilities: Vec<String>,
    },
    StateReset {
        plugin_id: String,
        base_revision: i64,
        state_revision: i64,
        scopes_digest: [u8; 32],
    },
    UiTrust {
        plugin_id: String,
        package_sha256: String,
        bundle_sha256: String,
        expected_revision: i64,
    },
}

struct ConsentEntry {
    actor_user_id: UserId,
    preparation_id: Uuid,
    expires_at: i64,
    payload: ConsentPayload,
}

#[derive(Default)]
pub(crate) struct ConsentStore {
    entries: Mutex<HashMap<[u8; 32], ConsentEntry>>,
}

impl ConsentStore {
    pub(crate) fn mint(
        &self,
        actor_user_id: UserId,
        preparation_id: Uuid,
        payload: ConsentPayload,
    ) -> (String, String) {
        let now = now_unix();
        let expires_at = now.saturating_add(CONSENT_TTL_SECS);
        let (token, digest) = mint_random_token();
        let mut guard = self.entries.lock();
        guard.retain(|_, e| e.expires_at >= now);
        if guard.len() >= MAX_TOKENS
            && let Some(oldest) = guard
                .iter()
                .min_by_key(|(_, e)| e.expires_at)
                .map(|(k, _)| *k)
        {
            guard.remove(&oldest);
        }
        guard.insert(
            digest,
            ConsentEntry {
                actor_user_id,
                preparation_id,
                expires_at,
                payload,
            },
        );
        (token, format_rfc3339(expires_at))
    }

    pub(crate) fn peek<R>(
        &self,
        actor_user_id: UserId,
        preparation_id: Option<Uuid>,
        token: &str,
        validate: impl FnOnce(&ConsentPayload) -> Result<R, PluginServiceError>,
    ) -> Result<R, PluginServiceError> {
        let mut guard = self.entries.lock();
        let (_, res) =
            validate_consent_entry(&mut guard, actor_user_id, preparation_id, token, validate)?;
        Ok(res)
    }

    pub(crate) fn peek_consume(
        &self,
        actor_user_id: UserId,
        preparation_id: Option<Uuid>,
        token: &str,
        validate: impl FnOnce(&ConsentPayload) -> Result<(), PluginServiceError>,
    ) -> Result<ConsentPayload, PluginServiceError> {
        let mut guard = self.entries.lock();
        let (digest, ()) =
            validate_consent_entry(&mut guard, actor_user_id, preparation_id, token, validate)?;
        let removed = guard.remove(&digest).ok_or_else(|| {
            PluginServiceError::conflict("plugin.consent_stale", "consent_token already used")
        })?;
        Ok(removed.payload)
    }

    pub(crate) fn mark_upgrade_pre_drained(
        &self,
        actor_user_id: UserId,
        preparation_id: Uuid,
        token: &str,
        expected_base_revision: i64,
        post_drain_revision: i64,
    ) {
        if token.trim().is_empty() {
            return;
        }
        let digest = sha256_token(token);
        let mut guard = self.entries.lock();
        if let Some(entry) = guard.get_mut(&digest)
            && entry.actor_user_id == actor_user_id
            && entry.preparation_id == preparation_id
            && let ConsentPayload::Upgrade {
                base_revision,
                pre_drained_to_revision,
                ..
            } = &mut entry.payload
            && *base_revision == expected_base_revision
            && pre_drained_to_revision.is_none()
        {
            *pre_drained_to_revision = Some(post_drain_revision);
        }
    }
}

fn validate_consent_entry<R>(
    guard: &mut HashMap<[u8; 32], ConsentEntry>,
    actor_user_id: UserId,
    preparation_id: Option<Uuid>,
    token: &str,
    validate: impl FnOnce(&ConsentPayload) -> Result<R, PluginServiceError>,
) -> Result<([u8; 32], R), PluginServiceError> {
    if token.trim().is_empty() {
        return Err(PluginServiceError::conflict(
            "plugin.consent_stale",
            "consent_token required",
        ));
    }
    let now = now_unix();
    let digest = sha256_token(token);
    guard.retain(|_, e| e.expires_at >= now);
    let Some(entry) = guard.get(&digest) else {
        return Err(PluginServiceError::conflict(
            "plugin.consent_stale",
            "consent_token missing or expired",
        ));
    };
    if entry.actor_user_id != actor_user_id
        || preparation_id.is_some_and(|pid| pid != entry.preparation_id)
        || entry.expires_at < now
    {
        return Err(PluginServiceError::conflict(
            "plugin.consent_stale",
            "consent_token does not match caller or preparation",
        ));
    }
    let out = validate(&entry.payload)?;
    Ok((digest, out))
}

pub(crate) fn digest_grants(grants: &[PluginGrant]) -> Result<[u8; 32], PluginServiceError> {
    let mut normalized = grants.to_vec();
    normalized.sort_by(|a, b| {
        a.capability
            .cmp(&b.capability)
            .then_with(|| format!("{:?}", a.scope).cmp(&format!("{:?}", b.scope)))
    });
    let json =
        serde_json::to_vec(&normalized).map_err(|e| PluginServiceError::Io(e.to_string()))?;
    Ok(Sha256::digest(&json).into())
}

pub(crate) fn digest_scopes(
    scopes: &[PluginSecurityScope],
) -> Result<[u8; 32], PluginServiceError> {
    let mut keys: Vec<String> = scopes.iter().map(|s| format!("{s:?}")).collect();
    keys.sort();
    keys.dedup();
    let json = serde_json::to_vec(&keys).map_err(|e| PluginServiceError::Io(e.to_string()))?;
    Ok(Sha256::digest(&json).into())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CleanupTokenBinding {
    pub actor_user_id: UserId,
    pub plugin_id: String,
    pub package_hash: String,
    pub generation: i64,
    pub plan_handle: String,
    pub revision: i64,
    pub plan_digest: String,
    pub expires_at: i64,
}

#[derive(Default)]
pub(crate) struct CleanupTokenStore {
    entries: Mutex<HashMap<[u8; 32], CleanupTokenBinding>>,
}

impl CleanupTokenStore {
    pub(crate) fn mint(&self, binding: CleanupTokenBinding) -> (String, String) {
        let now = now_unix();
        let (token, digest) = mint_random_token();
        let exp = binding.expires_at;
        let mut guard = self.entries.lock();
        guard.retain(|_, e| e.expires_at >= now);
        if guard.len() >= MAX_TOKENS
            && let Some(oldest) = guard
                .iter()
                .min_by_key(|(_, e)| e.expires_at)
                .map(|(k, _)| *k)
        {
            guard.remove(&oldest);
        }
        guard.insert(digest, binding);
        (token, format_rfc3339(exp))
    }

    pub(crate) fn verify(
        &self,
        actor_user_id: Option<UserId>,
        plugin_id: &str,
        package_hash: &str,
        generation: i64,
        plan_handle: &str,
        revision: i64,
        plan_digest: &str,
        token: &str,
        consume: bool,
    ) -> Result<(), PluginServiceError> {
        if token.trim().is_empty() {
            return Err(PluginServiceError::forbidden(
                "plugin.cleanup_token_required",
                "confirmation_token is required",
            ));
        }
        let now = now_unix();
        let digest = sha256_token(token);
        let mut guard = self.entries.lock();
        guard.retain(|_, e| e.expires_at >= now);
        let Some(entry) = guard.get(&digest) else {
            return Err(PluginServiceError::forbidden(
                "plugin.cleanup_token_invalid",
                "confirmation_token missing or expired",
            ));
        };
        if actor_user_id.is_some_and(|u| u != entry.actor_user_id)
            || entry.plugin_id != plugin_id
            || entry.package_hash != package_hash
            || entry.generation != generation
            || entry.plan_handle != plan_handle
            || entry.revision != revision
            || entry.plan_digest != plan_digest
            || entry.expires_at < now
        {
            return Err(PluginServiceError::conflict(
                "plugin.cleanup_token_mismatch",
                "confirmation_token does not match plan or plugin generation",
            ));
        }
        if consume {
            guard.remove(&digest);
        }
        Ok(())
    }
}
