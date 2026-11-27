use crate::{PackageError, SignatureStatus, SignatureVerification, TrustedKey};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use blake2::{Blake2b512, Digest as _};
use ring::signature::{ED25519, UnparsedPublicKey};
use sha2::Sha256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedPublicKey {
    pub key_id: String,
    pub fingerprint: String,
    pub key_id_bytes: [u8; 8],
    pub ed25519_pk: [u8; 32],
}

pub fn parse_minisign_public_key(raw: &str) -> Result<ParsedPublicKey, PackageError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(PackageError::InvalidSignature("empty public key".into()));
    }
    let lines: Vec<&str> = trimmed
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let b64_line = match lines.as_slice() {
        [single] => *single,
        [header, key_line] if header.starts_with("untrusted comment:") => *key_line,
        _ => {
            return Err(PackageError::InvalidSignature(
                "invalid minisign public key format".into(),
            ));
        }
    };
    let decoded = B64
        .decode(b64_line)
        .map_err(|e| PackageError::InvalidSignature(format!("invalid base64 public key: {e}")))?;
    if decoded.len() != 42 || &decoded[0..2] != b"Ed" {
        return Err(PackageError::InvalidSignature(
            "minisign public key must be 42 bytes with 'Ed' prefix".into(),
        ));
    }
    let mut key_id_bytes = [0u8; 8];
    key_id_bytes.copy_from_slice(&decoded[2..10]);
    let mut ed25519_pk = [0u8; 32];
    ed25519_pk.copy_from_slice(&decoded[10..42]);
    let key_id = format_key_id(&key_id_bytes);
    let fp = Sha256::digest(&decoded);
    Ok(ParsedPublicKey {
        key_id,
        fingerprint: hex_lower(fp.as_slice()),
        key_id_bytes,
        ed25519_pk,
    })
}

pub fn verify_minisign(
    package_sha256: &[u8; 32],
    sig_text: &str,
    trust_keys: &[TrustedKey],
) -> Result<SignatureVerification, PackageError> {
    let mut verification = verify_minisign_payload(package_sha256, sig_text, trust_keys)?;
    verification.package_sha256_hex = hex_lower(package_sha256);
    Ok(verification)
}

pub fn verify_minisign_payload(
    payload: &[u8],
    sig_text: &str,
    trust_keys: &[TrustedKey],
) -> Result<SignatureVerification, PackageError> {
    let parsed_sig = parse_signature(sig_text)?;
    let sig_key_id = format_key_id(&parsed_sig.key_id_bytes);
    let candidates = find_trusted_keys(&parsed_sig.key_id_bytes, &sig_key_id, trust_keys)?;

    let prehash;
    let main_msg: &[u8] = if parsed_sig.hashed {
        prehash = Blake2b512::digest(payload);
        prehash.as_slice()
    } else {
        payload
    };
    let mut global_msg = Vec::with_capacity(64 + parsed_sig.trusted_comment.len());
    global_msg.extend_from_slice(&parsed_sig.main_sig);
    global_msg.extend_from_slice(parsed_sig.trusted_comment.as_bytes());

    let mut last_err: Option<PackageError> = None;
    for matched in candidates {
        let pk = UnparsedPublicKey::new(&ED25519, &matched.ed25519_pk);
        if pk.verify(main_msg, &parsed_sig.main_sig).is_err() {
            last_err = Some(PackageError::InvalidSignature(
                "ed25519 signature verification failed".into(),
            ));
            continue;
        }
        if pk.verify(&global_msg, &parsed_sig.global_sig).is_err() {
            last_err = Some(PackageError::InvalidSignature(
                "trusted comment global signature verification failed".into(),
            ));
            continue;
        }
        let payload_sha = Sha256::digest(payload);
        return Ok(SignatureVerification {
            status: SignatureStatus::Trusted {
                key_id: matched.key_id,
                fingerprint: matched.fingerprint,
            },
            package_sha256_hex: hex_lower(payload_sha.as_slice()),
        });
    }

    Err(last_err.unwrap_or_else(|| {
        PackageError::InvalidSignature("ed25519 signature verification failed".into())
    }))
}

struct ParsedSignature {
    hashed: bool,
    key_id_bytes: [u8; 8],
    main_sig: [u8; 64],
    trusted_comment: String,
    global_sig: [u8; 64],
}

fn parse_signature(sig_text: &str) -> Result<ParsedSignature, PackageError> {
    if sig_text.trim().is_empty() {
        return Err(PackageError::InvalidSignature(
            "empty signature text".into(),
        ));
    }
    let normalized = sig_text.replace("\r\n", "\n");
    let raw_lines: Vec<&str> = normalized.split('\n').collect();
    let trimmed_lines: Vec<&str> = match raw_lines.last() {
        Some(&"") => raw_lines[..raw_lines.len() - 1].to_vec(),
        _ => raw_lines,
    };
    if trimmed_lines.len() != 4 {
        return Err(PackageError::InvalidSignature(format!(
            "expected 4 minisign lines, got {}",
            trimmed_lines.len()
        )));
    }
    if !trimmed_lines[0].starts_with("untrusted comment:") {
        return Err(PackageError::InvalidSignature(
            "missing untrusted comment line".into(),
        ));
    }
    let sig_bytes = B64.decode(trimmed_lines[1].trim()).map_err(|e| {
        PackageError::InvalidSignature(format!("invalid base64 signature line: {e}"))
    })?;
    if sig_bytes.len() != 74 {
        return Err(PackageError::InvalidSignature(format!(
            "signature payload must be 74 bytes, got {}",
            sig_bytes.len()
        )));
    }
    let hashed = match &sig_bytes[0..2] {
        b"ED" => true,
        b"Ed" => false,
        _ => {
            return Err(PackageError::InvalidSignature(
                "unsupported minisign algorithm tag".into(),
            ));
        }
    };
    let mut key_id_bytes = [0u8; 8];
    key_id_bytes.copy_from_slice(&sig_bytes[2..10]);
    let mut main_sig = [0u8; 64];
    main_sig.copy_from_slice(&sig_bytes[10..74]);

    let trusted_comment = trimmed_lines[2]
        .strip_prefix("trusted comment: ")
        .ok_or_else(|| PackageError::InvalidSignature("missing 'trusted comment: ' prefix".into()))?
        .to_string();

    let global_bytes = B64.decode(trimmed_lines[3].trim()).map_err(|e| {
        PackageError::InvalidSignature(format!("invalid base64 global signature: {e}"))
    })?;
    if global_bytes.len() != 64 {
        return Err(PackageError::InvalidSignature(format!(
            "global signature must be 64 bytes, got {}",
            global_bytes.len()
        )));
    }
    let mut global_sig = [0u8; 64];
    global_sig.copy_from_slice(&global_bytes);

    Ok(ParsedSignature {
        hashed,
        key_id_bytes,
        main_sig,
        trusted_comment,
        global_sig,
    })
}

fn find_trusted_keys(
    sig_key_id_bytes: &[u8; 8],
    sig_key_id: &str,
    trust_keys: &[TrustedKey],
) -> Result<Vec<ParsedPublicKey>, PackageError> {
    let mut candidates: Vec<ParsedPublicKey> = Vec::new();
    let mut parsed_entries: Vec<(bool, &str, Option<ParsedPublicKey>)> =
        Vec::with_capacity(trust_keys.len());

    for tk in trust_keys {
        let parsed = parse_minisign_public_key(&tk.public_key).ok();
        if !tk.revoked
            && let Some(ref p) = parsed
            && &p.key_id_bytes == sig_key_id_bytes
            && (tk.key_id.is_empty() || tk.key_id.eq_ignore_ascii_case(&p.key_id))
        {
            candidates.push(p.clone());
        }
        parsed_entries.push((tk.revoked, tk.key_id.as_str(), parsed));
    }

    for (revoked, tk_key_id, parsed) in &parsed_entries {
        if !revoked {
            continue;
        }
        if !tk_key_id.is_empty() && tk_key_id.eq_ignore_ascii_case(sig_key_id) {
            return Err(PackageError::KeyRevoked(sig_key_id.to_string()));
        }
        if let Some(p) = parsed
            && (&p.key_id_bytes == sig_key_id_bytes
                || candidates
                    .iter()
                    .any(|cand| cand.ed25519_pk == p.ed25519_pk))
        {
            return Err(PackageError::KeyRevoked(sig_key_id.to_string()));
        }
    }

    if candidates.is_empty() {
        return Err(PackageError::UntrustedKey(sig_key_id.to_string()));
    }
    Ok(candidates)
}

fn format_key_id(bytes: &[u8; 8]) -> String {
    format!("{:016X}", u64::from_le_bytes(*bytes))
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}
