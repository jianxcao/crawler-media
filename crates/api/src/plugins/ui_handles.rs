use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::reauth::now_unix;
use super::types::{PluginServiceError, format_rfc3339};

pub const UI_HANDLE_TTL_MS: i64 = 10 * 60 * 1000;
const HANDLE_PREFIX: &str = "uh1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VerifiedUiHandle {
    pub plugin_id: String,
    pub generation: i64,
    pub user_id: String,
    pub entity_kind: String,
    pub entity_id: String,
    pub revision: i64,
    pub exp_ms: i64,
}

#[derive(Serialize, Deserialize)]
struct HandleClaims<'a> {
    p: &'a str,
    g: i64,
    u: &'a str,
    k: &'a str,
    i: &'a str,
    r: i64,
    e: i64,
}

#[derive(Deserialize)]
struct OwnedHandleClaims {
    p: String,
    g: i64,
    u: String,
    k: String,
    i: String,
    r: i64,
    e: i64,
}

pub(crate) struct UiHandleSigner {
    key: [u8; 32],
}

impl Default for UiHandleSigner {
    fn default() -> Self {
        Self {
            key: super::reauth::random_256bit_entropy(),
        }
    }
}

impl UiHandleSigner {
    #[cfg(test)]
    pub(crate) fn from_key(key: [u8; 32]) -> Self {
        Self { key }
    }

    pub(crate) fn mint(
        &self,
        plugin_id: &str,
        generation: i64,
        user_id: &str,
        entity_kind: &str,
        entity_id: &str,
        revision: i64,
    ) -> Result<(String, String), PluginServiceError> {
        let now_ms = now_unix().saturating_mul(1000);
        self.mint_at(
            plugin_id,
            generation,
            user_id,
            entity_kind,
            entity_id,
            revision,
            now_ms,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mint_at(
        &self,
        plugin_id: &str,
        generation: i64,
        user_id: &str,
        entity_kind: &str,
        entity_id: &str,
        revision: i64,
        now_ms: i64,
    ) -> Result<(String, String), PluginServiceError> {
        let exp_ms = now_ms.saturating_add(UI_HANDLE_TTL_MS);
        let claims = HandleClaims {
            p: plugin_id,
            g: generation,
            u: user_id,
            k: entity_kind,
            i: entity_id,
            r: revision,
            e: exp_ms,
        };
        let payload_json =
            serde_json::to_vec(&claims).map_err(|e| PluginServiceError::Io(e.to_string()))?;
        let payload_b64 = URL_SAFE_NO_PAD.encode(payload_json);
        let mac = hmac_sha256(&self.key, payload_b64.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(mac);
        let token = format!("{HANDLE_PREFIX}.{payload_b64}.{sig_b64}");
        Ok((token, format_rfc3339(exp_ms / 1000)))
    }

    pub(crate) fn verify(
        &self,
        token: &str,
        expected_plugin_id: &str,
        expected_generation: i64,
        expected_user_id: &str,
        expected_kind: Option<&str>,
    ) -> Result<VerifiedUiHandle, PluginServiceError> {
        let now_ms = now_unix().saturating_mul(1000);
        self.verify_at(
            token,
            expected_plugin_id,
            expected_generation,
            expected_user_id,
            expected_kind,
            now_ms,
        )
    }

    pub(crate) fn verify_at(
        &self,
        token: &str,
        expected_plugin_id: &str,
        expected_generation: i64,
        expected_user_id: &str,
        expected_kind: Option<&str>,
        now_ms: i64,
    ) -> Result<VerifiedUiHandle, PluginServiceError> {
        let mut parts = token.split('.');
        let Some(prefix) = parts.next() else {
            return Err(PluginServiceError::invalid(
                "plugin.invalid_handle",
                "malformed UiHandle",
            ));
        };
        let Some(payload_b64) = parts.next() else {
            return Err(PluginServiceError::invalid(
                "plugin.invalid_handle",
                "malformed UiHandle",
            ));
        };
        let Some(sig_b64) = parts.next() else {
            return Err(PluginServiceError::invalid(
                "plugin.invalid_handle",
                "malformed UiHandle",
            ));
        };
        if parts.next().is_some()
            || prefix != HANDLE_PREFIX
            || payload_b64.is_empty()
            || sig_b64.is_empty()
        {
            return Err(PluginServiceError::invalid(
                "plugin.invalid_handle",
                "invalid UiHandle format",
            ));
        }

        let provided_sig = URL_SAFE_NO_PAD.decode(sig_b64).map_err(|_| {
            PluginServiceError::invalid("plugin.invalid_handle", "invalid UiHandle signature")
        })?;
        let expected_sig = hmac_sha256(&self.key, payload_b64.as_bytes());
        if !constant_time_eq(&provided_sig, &expected_sig) {
            return Err(PluginServiceError::forbidden(
                "plugin.forbidden",
                "UiHandle signature verification failed",
            ));
        }
        let payload_bytes = URL_SAFE_NO_PAD.decode(payload_b64).map_err(|_| {
            PluginServiceError::invalid("plugin.invalid_handle", "invalid UiHandle payload")
        })?;
        let claims: OwnedHandleClaims = serde_json::from_slice(&payload_bytes).map_err(|_| {
            PluginServiceError::invalid("plugin.invalid_handle", "invalid UiHandle claims")
        })?;

        if claims.e < now_ms {
            return Err(PluginServiceError::forbidden(
                "plugin.stale_handle",
                "UiHandle expired",
            ));
        }
        if claims.u != expected_user_id || claims.p != expected_plugin_id {
            return Err(PluginServiceError::forbidden(
                "plugin.forbidden",
                "UiHandle does not belong to caller or plugin",
            ));
        }
        if claims.g != expected_generation {
            return Err(PluginServiceError::conflict(
                "plugin.stale_generation",
                "UiHandle generation mismatch",
            ));
        }
        if let Some(kind) = expected_kind
            && claims.k != kind
        {
            return Err(PluginServiceError::invalid(
                "plugin.invalid_handle",
                format!("UiHandle kind `{}` does not match `{kind}`", claims.k),
            ));
        }
        Ok(VerifiedUiHandle {
            plugin_id: claims.p,
            generation: claims.g,
            user_id: claims.u,
            entity_kind: claims.k,
            entity_id: claims.i,
            revision: claims.r,
            exp_ms: claims.e,
        })
    }
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k_pad = [0u8; BLOCK];
    if key.len() > BLOCK {
        let hashed: [u8; 32] = Sha256::digest(key).into();
        k_pad[..32].copy_from_slice(&hashed);
    } else {
        k_pad[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k_pad[i];
        opad[i] ^= k_pad[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_ttl_scoping_and_tamper_checks() {
        let signer = UiHandleSigner::from_key([7u8; 32]);
        let (tok, _) = signer
            .mint_at("p1", 2, "u1", "site", "site-99", 5, 1_000_000)
            .unwrap();
        let ok = signer
            .verify_at(
                &tok,
                "p1",
                2,
                "u1",
                Some("site"),
                1_000_000 + UI_HANDLE_TTL_MS,
            )
            .unwrap();
        assert_eq!(ok.entity_id, "site-99");
        assert_eq!(ok.revision, 5);

        // Expired after 10 minutes + 1ms
        let exp_err = signer
            .verify_at(
                &tok,
                "p1",
                2,
                "u1",
                Some("site"),
                1_000_000 + UI_HANDLE_TTL_MS + 1,
            )
            .unwrap_err();
        assert_eq!(exp_err.status_and_code(), (403, "plugin.stale_handle"));

        // Cross-user / cross-plugin / cross-generation
        assert_eq!(
            signer
                .verify_at(&tok, "p1", 2, "u2", Some("site"), 1_000_000)
                .unwrap_err()
                .status_and_code()
                .0,
            403
        );
        assert_eq!(
            signer
                .verify_at(&tok, "p2", 2, "u1", Some("site"), 1_000_000)
                .unwrap_err()
                .status_and_code()
                .0,
            403
        );
        assert_eq!(
            signer
                .verify_at(&tok, "p1", 3, "u1", Some("site"), 1_000_000)
                .unwrap_err()
                .status_and_code()
                .0,
            409
        );

        // Raw UUID fails closed
        let raw_err = signer
            .verify_at(
                "00000000-0000-0000-0000-000000000001",
                "p1",
                2,
                "u1",
                Some("site"),
                1_000_000,
            )
            .unwrap_err();
        assert_eq!(raw_err.status_and_code().0, 400);
    }

    #[test]
    fn default_signer_key_has_no_fixed_uuid_v4_nibbles() {
        // Sampling 32 keys makes a false-positive match on a fixed nibble 16^-32.
        let samples: Vec<[u8; 32]> = (0..32).map(|_| UiHandleSigner::default().key).collect();
        assert!(
            samples.iter().any(|k| (k[6] >> 4) != 0x4),
            "byte 6 high nibble must not be fixed to UUIDv4 version 4"
        );
        assert!(
            samples.iter().any(|k| (k[22] >> 4) != 0x4),
            "byte 22 high nibble must not be fixed to UUIDv4 version 4"
        );
    }
}
