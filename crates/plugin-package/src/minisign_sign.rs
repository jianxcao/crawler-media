use crate::PackageError;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use blake2::digest::{Update, VariableOutput};
use blake2::{Blake2b512, Blake2bVar, Digest};
use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MinisignSigningKey {
    pub seed: [u8; 32],
    pub key_id_bytes: [u8; 8],
    pub public_key: [u8; 32],
}

impl MinisignSigningKey {
    pub fn key_id_hex(&self) -> String {
        format!("{:016X}", u64::from_le_bytes(self.key_id_bytes))
    }

    pub fn public_key_text(&self) -> String {
        let mut raw = Vec::with_capacity(42);
        raw.extend_from_slice(b"Ed");
        raw.extend_from_slice(&self.key_id_bytes);
        raw.extend_from_slice(&self.public_key);
        format!(
            "untrusted comment: minisign public key {}\n{}\n",
            self.key_id_hex(),
            B64.encode(raw)
        )
    }
}

pub fn parse_signing_key(raw: &str) -> Result<MinisignSigningKey, PackageError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(PackageError::InvalidSignature("empty signing key".into()));
    }
    if trimmed.len() == 64 && trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
        return parse_hex_seed_key(trimmed);
    }
    parse_minisign_secret_key_text(trimmed)
}

fn parse_hex_seed_key(hex: &str) -> Result<MinisignSigningKey, PackageError> {
    let mut seed = [0u8; 32];
    let (chunks, _) = hex.as_bytes().as_chunks::<2>();
    for (idx, chunk) in chunks.iter().enumerate() {
        let pair = std::str::from_utf8(chunk)
            .map_err(|_| PackageError::InvalidSignature("invalid hex seed".into()))?;
        seed[idx] = u8::from_str_radix(pair, 16)
            .map_err(|_| PackageError::InvalidSignature("invalid hex seed".into()))?;
    }
    let keypair = Ed25519KeyPair::from_seed_unchecked(&seed)
        .map_err(|_| PackageError::InvalidSignature("invalid ed25519 seed".into()))?;
    let mut key_id_bytes = [0u8; 8];
    let mut key_hasher = Blake2bVar::new(8)
        .map_err(|e| PackageError::InvalidSignature(format!("blake2 init failed: {e}")))?;
    key_hasher.update(&seed);
    key_hasher
        .finalize_variable(&mut key_id_bytes)
        .map_err(|e| PackageError::InvalidSignature(format!("blake2 finalize failed: {e}")))?;

    let mut public_key = [0u8; 32];
    public_key.copy_from_slice(keypair.public_key().as_ref());
    Ok(MinisignSigningKey {
        seed,
        key_id_bytes,
        public_key,
    })
}

fn parse_minisign_secret_key_text(raw: &str) -> Result<MinisignSigningKey, PackageError> {
    let lines: Vec<&str> = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let b64_line = match lines.as_slice() {
        [single] => *single,
        [header, key_line] if header.starts_with("untrusted comment:") => *key_line,
        _ => {
            return Err(PackageError::InvalidSignature(
                "invalid signing key: expected 64-hex Ed25519 seed or 2-line minisign secret key"
                    .into(),
            ));
        }
    };
    let decoded = B64
        .decode(b64_line)
        .map_err(|e| PackageError::InvalidSignature(format!("invalid base64 secret key: {e}")))?;
    if decoded.len() != 158 || &decoded[0..2] != b"Ed" {
        return Err(PackageError::InvalidSignature(
            "minisign secret key must be 158 bytes with 'Ed' prefix".into(),
        ));
    }
    let kdf_alg = &decoded[2..4];
    if kdf_alg == b"Sc" {
        return Err(PackageError::InvalidSignature(
            "encrypted minisign secret keys are unsupported; supply unencrypted key or 64-hex seed"
                .into(),
        ));
    }
    if kdf_alg != [0u8, 0u8] || &decoded[4..6] != b"B2" {
        return Err(PackageError::InvalidSignature(
            "unsupported minisign secret key KDF/checksum header".into(),
        ));
    }

    let key_id_bytes: [u8; 8] = decoded[54..62]
        .try_into()
        .map_err(|_| PackageError::InvalidSignature("invalid minisign key id".into()))?;
    let sk_64 = &decoded[62..126];
    let expected_cksum = &decoded[126..158];

    let mut cksum_hasher = Blake2bVar::new(32)
        .map_err(|e| PackageError::InvalidSignature(format!("blake2 init failed: {e}")))?;
    cksum_hasher.update(b"Ed");
    cksum_hasher.update(&key_id_bytes);
    cksum_hasher.update(sk_64);
    let mut actual_cksum = [0u8; 32];
    cksum_hasher
        .finalize_variable(&mut actual_cksum)
        .map_err(|e| PackageError::InvalidSignature(format!("blake2 finalize failed: {e}")))?;
    if actual_cksum != expected_cksum {
        return Err(PackageError::InvalidSignature(
            "minisign secret key checksum mismatch".into(),
        ));
    }

    let mut seed = [0u8; 32];
    seed.copy_from_slice(&sk_64[..32]);
    let mut pk_from_file = [0u8; 32];
    pk_from_file.copy_from_slice(&sk_64[32..]);

    let keypair = Ed25519KeyPair::from_seed_unchecked(&seed)
        .map_err(|_| PackageError::InvalidSignature("invalid ed25519 secret key bytes".into()))?;
    if keypair.public_key().as_ref() != pk_from_file {
        return Err(PackageError::InvalidSignature(
            "minisign secret key public key does not match seed".into(),
        ));
    }
    Ok(MinisignSigningKey {
        seed,
        key_id_bytes,
        public_key: pk_from_file,
    })
}

pub fn sign_cmplugin_bytes(
    cmplugin_bytes: &[u8],
    key: &MinisignSigningKey,
    filename_hint: &str,
) -> Result<String, PackageError> {
    let package_sha256: [u8; 32] = Sha256::digest(cmplugin_bytes).into();
    let prehash = Blake2b512::digest(package_sha256);
    let keypair = Ed25519KeyPair::from_seed_unchecked(&key.seed)
        .map_err(|_| PackageError::InvalidSignature("invalid ed25519 signing seed".into()))?;
    let main_sig = keypair.sign(prehash.as_slice());

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let safe_name: String = filename_hint
        .chars()
        .map(|c| if c.is_control() || c == '\t' { '_' } else { c })
        .collect();
    let file_label = if safe_name.trim().is_empty() {
        "plugin.cmplugin"
    } else {
        safe_name.as_str()
    };
    let trusted_comment = format!("timestamp:{ts}\tfile:{file_label}\thashed");

    let mut global_msg = Vec::with_capacity(64 + trusted_comment.len());
    global_msg.extend_from_slice(main_sig.as_ref());
    global_msg.extend_from_slice(trusted_comment.as_bytes());
    let global_sig = keypair.sign(&global_msg);

    let mut sig_payload = Vec::with_capacity(74);
    sig_payload.extend_from_slice(b"ED");
    sig_payload.extend_from_slice(&key.key_id_bytes);
    sig_payload.extend_from_slice(main_sig.as_ref());

    Ok(format!(
        "untrusted comment: signature from minisign secret key\n{}\ntrusted comment: {}\n{}\n",
        B64.encode(sig_payload),
        trusted_comment,
        B64.encode(global_sig.as_ref()),
    ))
}
