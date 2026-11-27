//! Minimal bencode walk to compute a .torrent's v1 info hash (D-H2).

use std::collections::HashSet;

use sha1::{Digest, Sha1};
use sha2::Sha256;

pub const MAX_TORRENT_BYTES: usize = 10 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedTorrentPayload {
    pub bytes: Vec<u8>,
    pub bytes_digest: String,
    pub info_hash: String,
    pub verified_total_bytes: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TorrentParseError {
    #[error("empty torrent payload")]
    Empty,
    #[error("torrent payload exceeds max size of {limit} bytes")]
    PayloadTooLarge { limit: usize },
    #[error("malformed bencode at offset {offset}: {reason}")]
    MalformedBencode { offset: usize, reason: &'static str },
    #[error("bencode nesting depth exceeds {max}")]
    NestingTooDeep { max: usize },
    #[error("missing or non-dictionary info entry")]
    InvalidInfoDict,
    #[error("torrent info dictionary missing valid length or files")]
    MissingLength,
    #[error("torrent contains both info.length and info.files")]
    AmbiguousLayout,
    #[error("torrent file list is empty")]
    EmptyFiles,
    #[error("negative file length: {0}")]
    NegativeLength(i64),
    #[error("invalid file length entry")]
    InvalidFileEntry,
    #[error("torrent total byte length overflows signed 64-bit integer")]
    LengthOverflow,
}

pub fn parse_verified_torrent(bytes: &[u8]) -> Result<VerifiedTorrentPayload, TorrentParseError> {
    if bytes.is_empty() {
        return Err(TorrentParseError::Empty);
    }
    if bytes.len() > MAX_TORRENT_BYTES {
        return Err(TorrentParseError::PayloadTooLarge {
            limit: MAX_TORRENT_BYTES,
        });
    }
    if bytes.first() != Some(&b'd') {
        return Err(TorrentParseError::MalformedBencode {
            offset: 0,
            reason: "root must be a bencode dictionary",
        });
    }
    let (info_total, end) = scan_top_dict(bytes)?;
    if end != bytes.len() {
        return Err(TorrentParseError::MalformedBencode {
            offset: end,
            reason: "trailing bytes after root dictionary",
        });
    }
    let (info_start, info_end, verified_total_bytes) =
        info_total.ok_or(TorrentParseError::InvalidInfoDict)?;
    let info_hash = hex_lower(&Sha1::digest(&bytes[info_start..info_end]));
    let bytes_digest = hex_lower(&Sha256::digest(bytes));
    Ok(VerifiedTorrentPayload {
        bytes: bytes.to_vec(),
        bytes_digest,
        info_hash,
        verified_total_bytes,
    })
}

fn hex_lower(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

type InfoScan = (usize, usize, i64);

fn scan_top_dict(bytes: &[u8]) -> Result<(Option<InfoScan>, usize), TorrentParseError> {
    let mut i = 1;
    let mut info_scan = None;
    let mut seen = HashSet::new();
    loop {
        let b = *bytes.get(i).ok_or(TorrentParseError::MalformedBencode {
            offset: i,
            reason: "unterminated root dictionary",
        })?;
        if b == b'e' {
            return Ok((info_scan, i + 1));
        }
        let (key, val_start) = read_strict_bytes(bytes, i)?;
        if !seen.insert(key) {
            return Err(TorrentParseError::MalformedBencode {
                offset: i,
                reason: "duplicate dictionary key",
            });
        }
        if key == b"info" {
            let val_first = *bytes
                .get(val_start)
                .ok_or(TorrentParseError::MalformedBencode {
                    offset: val_start,
                    reason: "unterminated root dictionary",
                })?;
            if val_first != b'd' {
                return Err(TorrentParseError::InvalidInfoDict);
            }
            let (total, val_end) = parse_info_total_bytes(bytes, val_start)?;
            info_scan = Some((val_start, val_end, total));
            i = val_end;
        } else {
            i = validate_bencode(bytes, val_start, 1)?;
        }
    }
}

fn parse_info_total_bytes(
    bytes: &[u8],
    info_start: usize,
) -> Result<(i64, usize), TorrentParseError> {
    let mut i = info_start + 1;
    let mut single_len: Option<i64> = None;
    let mut files_len: Option<i64> = None;
    let mut seen = HashSet::new();
    let end;
    loop {
        let b = *bytes.get(i).ok_or(TorrentParseError::MalformedBencode {
            offset: i,
            reason: "unterminated info dictionary",
        })?;
        if b == b'e' {
            end = i + 1;
            break;
        }
        let (key, val_start) = read_strict_bytes(bytes, i)?;
        if !seen.insert(key) {
            return Err(TorrentParseError::MalformedBencode {
                offset: i,
                reason: "duplicate key in info dictionary",
            });
        }
        if key == b"length" {
            let (len, next) = read_length_int(bytes, val_start)?;
            single_len = Some(len);
            i = next;
        } else if key == b"files" {
            let (total, next) = parse_files_list(bytes, val_start, 2)?;
            files_len = Some(total);
            i = next;
        } else {
            i = validate_bencode(bytes, val_start, 2)?;
        }
    }
    let total = match (single_len, files_len) {
        (Some(_), Some(_)) => return Err(TorrentParseError::AmbiguousLayout),
        (Some(len), None) | (None, Some(len)) => len,
        (None, None) => return Err(TorrentParseError::MissingLength),
    };
    Ok((total, end))
}

fn parse_files_list(
    bytes: &[u8],
    start: usize,
    depth: usize,
) -> Result<(i64, usize), TorrentParseError> {
    if depth >= MAX_DEPTH {
        return Err(TorrentParseError::NestingTooDeep { max: MAX_DEPTH });
    }
    if bytes.get(start) != Some(&b'l') {
        return Err(TorrentParseError::MalformedBencode {
            offset: start,
            reason: "info.files must be a list",
        });
    }
    let mut i = start + 1;
    let mut count = 0usize;
    let mut total = 0i64;
    loop {
        let b = *bytes.get(i).ok_or(TorrentParseError::MalformedBencode {
            offset: i,
            reason: "unterminated info.files list",
        })?;
        if b == b'e' {
            if count == 0 {
                return Err(TorrentParseError::EmptyFiles);
            }
            return Ok((total, i + 1));
        }
        let (len, next) = parse_file_entry(bytes, i, depth + 1)?;
        total = total
            .checked_add(len)
            .ok_or(TorrentParseError::LengthOverflow)?;
        count += 1;
        i = next;
    }
}

fn parse_file_entry(
    bytes: &[u8],
    start: usize,
    depth: usize,
) -> Result<(i64, usize), TorrentParseError> {
    if depth >= MAX_DEPTH {
        return Err(TorrentParseError::NestingTooDeep { max: MAX_DEPTH });
    }
    if bytes.get(start) != Some(&b'd') {
        return Err(TorrentParseError::InvalidFileEntry);
    }
    let mut i = start + 1;
    let mut file_len: Option<i64> = None;
    let mut seen = HashSet::new();
    loop {
        let b = *bytes.get(i).ok_or(TorrentParseError::MalformedBencode {
            offset: i,
            reason: "unterminated file dictionary",
        })?;
        if b == b'e' {
            let len = file_len.ok_or(TorrentParseError::InvalidFileEntry)?;
            return Ok((len, i + 1));
        }
        let (key, val_start) = read_strict_bytes(bytes, i)?;
        if !seen.insert(key) {
            return Err(TorrentParseError::MalformedBencode {
                offset: i,
                reason: "duplicate key in file entry",
            });
        }
        if key == b"length" {
            let (len, next) = read_length_int(bytes, val_start)?;
            file_len = Some(len);
            i = next;
        } else {
            i = validate_bencode(bytes, val_start, depth + 1)?;
        }
    }
}

fn read_length_int(bytes: &[u8], start: usize) -> Result<(i64, usize), TorrentParseError> {
    let (val, next) = read_strict_int(bytes, start, true)?;
    if val < 0 {
        return Err(TorrentParseError::NegativeLength(val));
    }
    Ok((val, next))
}

fn read_strict_bytes(bytes: &[u8], start: usize) -> Result<(&[u8], usize), TorrentParseError> {
    let slice = bytes
        .get(start..)
        .ok_or(TorrentParseError::MalformedBencode {
            offset: start,
            reason: "unexpected eof reading byte string",
        })?;
    let colon_rel =
        slice
            .iter()
            .position(|b| *b == b':')
            .ok_or(TorrentParseError::MalformedBencode {
                offset: start,
                reason: "missing byte string colon",
            })?;
    let len_bytes = &slice[..colon_rel];
    if len_bytes.is_empty()
        || !len_bytes.iter().all(u8::is_ascii_digit)
        || (len_bytes.len() > 1 && len_bytes[0] == b'0')
    {
        return Err(TorrentParseError::MalformedBencode {
            offset: start,
            reason: "invalid byte string length prefix",
        });
    }
    let len_str =
        std::str::from_utf8(len_bytes).map_err(|_| TorrentParseError::MalformedBencode {
            offset: start,
            reason: "non-utf8 byte string length",
        })?;
    let len: usize = len_str
        .parse()
        .map_err(|_| TorrentParseError::MalformedBencode {
            offset: start,
            reason: "byte string length overflow",
        })?;
    let data_start = start + colon_rel + 1;
    let data_end = data_start
        .checked_add(len)
        .ok_or(TorrentParseError::MalformedBencode {
            offset: start,
            reason: "byte string end overflow",
        })?;
    let data = bytes
        .get(data_start..data_end)
        .ok_or(TorrentParseError::MalformedBencode {
            offset: start,
            reason: "truncated byte string",
        })?;
    Ok((data, data_end))
}

fn read_strict_int(
    bytes: &[u8],
    start: usize,
    for_length: bool,
) -> Result<(i64, usize), TorrentParseError> {
    if bytes.get(start) != Some(&b'i') {
        return Err(TorrentParseError::MalformedBencode {
            offset: start,
            reason: "expected bencode integer",
        });
    }
    let rest = bytes
        .get(start + 1..)
        .ok_or(TorrentParseError::MalformedBencode {
            offset: start,
            reason: "unterminated bencode integer",
        })?;
    let end_rel =
        rest.iter()
            .position(|b| *b == b'e')
            .ok_or(TorrentParseError::MalformedBencode {
                offset: start,
                reason: "missing integer terminator",
            })?;
    let raw = &rest[..end_rel];
    validate_int_digits(raw, start)?;
    let raw_str = std::str::from_utf8(raw).map_err(|_| TorrentParseError::MalformedBencode {
        offset: start,
        reason: "non-utf8 integer",
    })?;
    let parsed: i64 = raw_str.parse().map_err(|_| {
        if for_length {
            TorrentParseError::LengthOverflow
        } else {
            TorrentParseError::MalformedBencode {
                offset: start,
                reason: "integer out of i64 range",
            }
        }
    })?;
    Ok((parsed, start + 1 + end_rel + 1))
}

fn validate_int_digits(raw: &[u8], offset: usize) -> Result<(), TorrentParseError> {
    if raw.is_empty() {
        return Err(TorrentParseError::MalformedBencode {
            offset,
            reason: "empty integer",
        });
    }
    if raw == b"-0" {
        return Err(TorrentParseError::MalformedBencode {
            offset,
            reason: "negative zero integer",
        });
    }
    let digits = raw.strip_prefix(b"-").unwrap_or(raw);
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return Err(TorrentParseError::MalformedBencode {
            offset,
            reason: "invalid integer digits",
        });
    }
    if digits.len() > 1 && digits[0] == b'0' {
        return Err(TorrentParseError::MalformedBencode {
            offset,
            reason: "leading zero in integer",
        });
    }
    Ok(())
}

fn validate_bencode(bytes: &[u8], start: usize, depth: usize) -> Result<usize, TorrentParseError> {
    let b = *bytes
        .get(start)
        .ok_or(TorrentParseError::MalformedBencode {
            offset: start,
            reason: "unexpected eof",
        })?;
    match b {
        b'i' => read_strict_int(bytes, start, false).map(|(_, next)| next),
        b'0'..=b'9' => read_strict_bytes(bytes, start).map(|(_, next)| next),
        b'l' => {
            if depth >= MAX_DEPTH {
                return Err(TorrentParseError::NestingTooDeep { max: MAX_DEPTH });
            }
            let mut i = start + 1;
            loop {
                let cur = *bytes.get(i).ok_or(TorrentParseError::MalformedBencode {
                    offset: i,
                    reason: "unterminated list",
                })?;
                if cur == b'e' {
                    return Ok(i + 1);
                }
                i = validate_bencode(bytes, i, depth + 1)?;
            }
        }
        b'd' => {
            if depth >= MAX_DEPTH {
                return Err(TorrentParseError::NestingTooDeep { max: MAX_DEPTH });
            }
            let mut i = start + 1;
            let mut seen = HashSet::new();
            loop {
                let cur = *bytes.get(i).ok_or(TorrentParseError::MalformedBencode {
                    offset: i,
                    reason: "unterminated dict",
                })?;
                if cur == b'e' {
                    return Ok(i + 1);
                }
                let (key, val_start) = read_strict_bytes(bytes, i)?;
                if !seen.insert(key) {
                    return Err(TorrentParseError::MalformedBencode {
                        offset: i,
                        reason: "duplicate dict key",
                    });
                }
                i = validate_bencode(bytes, val_start, depth + 1)?;
            }
        }
        _ => Err(TorrentParseError::MalformedBencode {
            offset: start,
            reason: "unexpected bencode token",
        }),
    }
}

/// SHA-1 of the raw `info` dictionary, lowercase hex; `None` if `bytes` is
/// not a bencoded dictionary with an `info` entry.
pub fn torrent_info_hash(bytes: &[u8]) -> Option<String> {
    if bytes.first() != Some(&b'd') {
        return None;
    }
    let mut i = 1;
    while *bytes.get(i)? != b'e' {
        let (key, after_key) = read_string(bytes, i)?;
        let end = skip(bytes, after_key, 0)?;
        if key == b"info" {
            let digest = Sha1::digest(&bytes[after_key..end]);
            return Some(digest.iter().map(|b| format!("{b:02x}")).collect());
        }
        i = end;
    }
    None
}

fn read_string(bytes: &[u8], i: usize) -> Option<(&[u8], usize)> {
    let colon = i + bytes.get(i..)?.iter().position(|b| *b == b':')?;
    let len: usize = std::str::from_utf8(&bytes[i..colon]).ok()?.parse().ok()?;
    let start = colon + 1;
    let end = start.checked_add(len)?;
    Some((bytes.get(start..end)?, end))
}

/// Review M-5: deepest list/dict nesting accepted. Real torrents nest a few
/// levels; site responses are untrusted, so recursion is bounded.
const MAX_DEPTH: usize = 64;

/// End index (exclusive) of the bencoded value starting at `i`.
fn skip(bytes: &[u8], i: usize, depth: usize) -> Option<usize> {
    match *bytes.get(i)? {
        b'i' => Some(i + bytes.get(i..)?.iter().position(|b| *b == b'e')? + 1),
        b'l' | b'd' if depth < MAX_DEPTH => {
            let mut j = i + 1;
            while *bytes.get(j)? != b'e' {
                j = skip(bytes, j, depth + 1)?;
            }
            Some(j + 1)
        }
        b'0'..=b'9' => read_string(bytes, i).map(|(_, end)| end),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::torrent_info_hash;

    #[test]
    fn hashes_exactly_the_raw_info_dictionary() {
        let info =
            b"d6:lengthi12e4:name8:demo.mkv12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae";
        let mut torrent = b"d8:announce16:https://t.ex/ann4:info".to_vec();
        torrent.extend_from_slice(info);
        torrent.extend_from_slice(b"e");
        use sha1::Digest;
        let expected: String = sha1::Sha1::digest(info)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            torrent_info_hash(&torrent).as_deref(),
            Some(expected.as_str())
        );
        // Known vector: SHA-1 of "d4:name1:ae".
        assert_eq!(
            torrent_info_hash(b"d4:infod4:name1:aee").as_deref(),
            Some(sha1_hex(b"d4:name1:ae").as_str())
        );
    }

    fn sha1_hex(data: &[u8]) -> String {
        use sha1::Digest;
        sha1::Sha1::digest(data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Review M-5: hostile nesting must not overflow the worker stack.
    #[test]
    fn deeply_nested_info_is_rejected_without_overflow() {
        let depth = 100_000;
        let mut torrent = b"d4:info".to_vec();
        torrent.extend(std::iter::repeat_n(b'l', depth));
        torrent.extend(std::iter::repeat_n(b'e', depth));
        torrent.push(b'e');
        assert_eq!(torrent_info_hash(&torrent), None);
        // Ordinary nesting still works.
        assert!(torrent_info_hash(b"d4:infod5:filesld4:pathl1:aeeeee").is_some());
    }

    #[test]
    fn rejects_non_torrents() {
        assert_eq!(torrent_info_hash(b"<html>"), None);
        assert_eq!(torrent_info_hash(b"d8:announce3:abce"), None);
        assert_eq!(torrent_info_hash(b"d4:infod4:name"), None);
    }
}
