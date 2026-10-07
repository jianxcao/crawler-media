use crate::DownloaderError;
use sha2::{Digest, Sha256};

/// Persistent submission identity, derived only from the stable enclosure.
/// Unlike the generic application tag, this mark proves which URL was submitted.
pub fn ownership_tag(enclosure: &str) -> String {
    format!(
        "crawler-media-owned-{:x}",
        Sha256::digest(enclosure.as_bytes())
    )
}

/// Strict BEP-9 btih parsing, supporting hexadecimal and base32 SHA-1 forms.
/// Conflicting or malformed hash parameters fail closed rather than guessing.
pub fn magnet_info_hash(enclosure: &str) -> Option<String> {
    if !enclosure.get(..8)?.eq_ignore_ascii_case("magnet:?") {
        return None;
    }
    let mut found = None;
    for parameter in enclosure[8..].split('&') {
        let Some((key, value)) = parameter.split_once('=') else {
            continue;
        };
        if !key.eq_ignore_ascii_case("xt") {
            continue;
        }
        let Some(value) = percent_decode(value) else {
            continue;
        };
        if !value
            .get(..9)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("urn:btih:"))
        {
            continue;
        }
        let Some(hash) = normalize_hash(&value[9..]) else {
            return None;
        };
        if found.as_ref().is_some_and(|existing| existing != &hash) {
            return None;
        }
        found = Some(hash);
    }
    found
}

pub(crate) fn normalize_hash(value: &str) -> Option<String> {
    if value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(value.to_ascii_lowercase());
    }
    if value.len() != 32 {
        return None;
    }
    let mut bits = 0u32;
    let mut count = 0;
    let mut bytes = Vec::with_capacity(20);
    for ch in value.bytes() {
        let number = match ch.to_ascii_uppercase() {
            b'A'..=b'Z' => ch.to_ascii_uppercase() - b'A',
            b'2'..=b'7' => ch - b'2' + 26,
            _ => return None,
        };
        bits = (bits << 5) | u32::from(number);
        count += 5;
        if count >= 8 {
            count -= 8;
            bytes.push(((bits >> count) & 255) as u8);
        }
    }
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn percent_decode(value: &str) -> Option<String> {
    let mut bytes = Vec::new();
    let mut chars = value.bytes();
    while let Some(ch) = chars.next() {
        if ch == b'%' {
            let a = char::from(chars.next()?).to_digit(16)?;
            let b = char::from(chars.next()?).to_digit(16)?;
            bytes.push((a * 16 + b) as u8);
        } else {
            bytes.push(ch);
        }
    }
    String::from_utf8(bytes).ok()
}

pub(crate) fn proven_hashes(hashes: Vec<String>) -> Result<Option<String>, DownloaderError> {
    match hashes.len() {
        0 => Ok(None),
        1 => normalize_hash(&hashes[0])
            .map(Some)
            .ok_or_else(|| unproven("invalid actual infohash")),
        _ => Err(unproven("ambiguous persistent ownership marks")),
    }
}

/// Live identity is required before a destructive delete. Magnet / session-known
/// hashes that are already gone are a no-op; unproven HTTP identity is refused.
pub(crate) fn owned_removal_target(
    live_identity: Result<Option<String>, DownloaderError>,
    enclosure: &str,
    session_known: bool,
) -> Result<Option<String>, DownloaderError> {
    match live_identity? {
        Some(hash) => Ok(Some(hash)),
        None if magnet_info_hash(enclosure).is_some() || session_known => Ok(None),
        None => Err(unproven(
            "HTTP enclosure could not prove an actual downloader identity",
        )),
    }
}

pub fn unproven(reason: &str) -> DownloaderError {
    tracing::error!(%reason, "refusing downloader cleanup without exact identity");
    DownloaderError::Message(format!(
        "owned identity unproven: {reason}; refusing unsafe cleanup"
    ))
}
