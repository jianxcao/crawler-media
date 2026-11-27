use crate::minisign::hex_lower;
use crate::{ArchiveLimits, PackageError};
use crc32fast::Hasher as Crc32;
use flate2::read::DeflateDecoder;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use unicode_normalization::UnicodeNormalization;

pub(crate) struct ExtractedEntry {
    pub path: String,
    pub size: u64,
    pub sha256_hex: String,
    pub bytes: Vec<u8>,
}

const FORBIDDEN_EXEC_EXTS: &[&str] = &[
    "sh", "bat", "cmd", "exe", "dll", "so", "dylib", "ps1", "py", "rb", "pl",
];

pub(crate) struct BoundedPackageBytes {
    pub raw: Vec<u8>,
    pub package_sha256: [u8; 32],
    pub package_sha256_hex: String,
}

pub(crate) fn read_bounded_bytes<R: Read>(
    mut reader: R,
    limits: &ArchiveLimits,
) -> Result<BoundedPackageBytes, PackageError> {
    let mut raw = Vec::new();
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(PackageError::InvalidArchive(format!(
                    "failed reading package stream: {e}"
                )));
            }
        };
        if n == 0 {
            break;
        }
        let next_len = (raw.len() as u64).saturating_add(n as u64);
        if next_len > limits.max_package_bytes {
            return Err(PackageError::LimitExceeded(format!(
                "compressed package exceeds {} bytes",
                limits.max_package_bytes
            )));
        }
        hasher.update(&buf[..n]);
        raw.extend_from_slice(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut package_sha256 = [0u8; 32];
    package_sha256.copy_from_slice(digest.as_slice());
    let package_sha256_hex = hex_lower(&package_sha256);
    Ok(BoundedPackageBytes {
        raw,
        package_sha256,
        package_sha256_hex,
    })
}

pub(crate) fn extract_zip_entries(
    raw: &[u8],
    limits: &ArchiveLimits,
) -> Result<BTreeMap<String, ExtractedEntry>, PackageError> {
    if raw.len() < 22 {
        return Err(PackageError::InvalidArchive(
            "package is empty or too small to be a ZIP archive".into(),
        ));
    }
    let (cd_records, cd_offset) = parse_central_directory(raw, limits)?;
    validate_lfh_layout(raw, &cd_records, cd_offset)?;
    extract_all_entries(raw, &cd_records, limits)
}

struct CdRecord {
    name: String,
    is_dir: bool,
    method: u16,
    flags: u16,
    crc32: u32,
    compressed_size: u32,
    uncompressed_size: u32,
    lfh_offset: usize,
}

fn find_eocd(data: &[u8]) -> Result<usize, PackageError> {
    let min_pos = data.len().saturating_sub(22 + 65535);
    let mut pos = data.len() - 22;
    loop {
        if data[pos..pos + 4] == [0x50, 0x4b, 0x05, 0x06] {
            let comment_len = u16::from_le_bytes([data[pos + 20], data[pos + 21]]) as usize;
            if pos + 22 + comment_len == data.len() {
                return Ok(pos);
            }
        }
        if pos == min_pos {
            break;
        }
        pos -= 1;
    }
    Err(PackageError::InvalidArchive(
        "missing ZIP end of central directory record".into(),
    ))
}

fn parse_central_directory(
    data: &[u8],
    limits: &ArchiveLimits,
) -> Result<(Vec<CdRecord>, usize), PackageError> {
    let eocd = find_eocd(data)?;
    let disk = le_u16(data, eocd + 4)?;
    let cd_disk = le_u16(data, eocd + 6)?;
    let disk_entries = le_u16(data, eocd + 8)? as usize;
    let total_entries = le_u16(data, eocd + 10)? as usize;
    let cd_size = le_u32(data, eocd + 12)? as usize;
    let cd_offset = le_u32(data, eocd + 16)? as usize;

    if disk != 0 || cd_disk != 0 || disk_entries != total_entries {
        return Err(PackageError::InvalidArchive(
            "multi-disk ZIP archives are not supported".into(),
        ));
    }
    if total_entries == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_offset == 0xFFFF_FFFF {
        return Err(PackageError::InvalidArchive(
            "ZIP64 archives are not supported".into(),
        ));
    }
    if total_entries > limits.max_entries {
        return Err(PackageError::LimitExceeded(format!(
            "archive entry count {total_entries} exceeds limit {}",
            limits.max_entries
        )));
    }
    let cd_end = cd_offset
        .checked_add(cd_size)
        .filter(|&end| end == eocd)
        .ok_or_else(|| PackageError::InvalidArchive("invalid central directory bounds".into()))?;

    let mut pos = cd_offset;
    let mut records = Vec::with_capacity(total_entries);
    for _ in 0..total_entries {
        if pos + 46 > cd_end || data[pos..pos + 4] != [0x50, 0x4b, 0x01, 0x02] {
            return Err(PackageError::InvalidArchive(
                "corrupt central directory header".into(),
            ));
        }
        let flags = le_u16(data, pos + 8)?;
        let method = le_u16(data, pos + 10)?;
        let crc32 = le_u32(data, pos + 16)?;
        let compressed_size = le_u32(data, pos + 20)?;
        let uncompressed_size = le_u32(data, pos + 24)?;
        let name_len = le_u16(data, pos + 28)? as usize;
        let extra_len = le_u16(data, pos + 30)? as usize;
        let comment_len = le_u16(data, pos + 32)? as usize;
        let external_attr = le_u32(data, pos + 38)?;
        let lfh_offset = le_u32(data, pos + 42)? as usize;

        let header_end = pos + 46 + name_len + extra_len + comment_len;
        if header_end > cd_end {
            return Err(PackageError::InvalidArchive(
                "central directory entry exceeds bounds".into(),
            ));
        }
        let raw_name = &data[pos + 46..pos + 46 + name_len];
        let extra = &data[pos + 46 + name_len..pos + 46 + name_len + extra_len];
        let name = std::str::from_utf8(raw_name)
            .map_err(|_| PackageError::UnsafePath {
                path: String::from_utf8_lossy(raw_name).into_owned(),
                reason: "non-UTF-8 entry name".into(),
            })?
            .to_string();

        check_entry_security(&name, flags, method, external_attr, extra)?;
        let is_dir = name.ends_with('/');
        records.push(CdRecord {
            name,
            is_dir,
            method,
            flags,
            crc32,
            compressed_size,
            uncompressed_size,
            lfh_offset,
        });
        pos = header_end;
    }
    if pos != cd_end {
        return Err(PackageError::InvalidArchive(
            "central directory size mismatch".into(),
        ));
    }
    Ok((records, cd_offset))
}

fn validate_lfh_layout(
    data: &[u8],
    records: &[CdRecord],
    cd_offset: usize,
) -> Result<(), PackageError> {
    if records.is_empty() {
        if cd_offset != 0 {
            return Err(PackageError::InvalidArchive(
                "unexpected bytes before central directory".into(),
            ));
        }
        return Ok(());
    }
    let mut spans: Vec<(usize, usize, bool)> = Vec::with_capacity(records.len());
    for rec in records {
        let lfh = rec.lfh_offset;
        if lfh + 30 > cd_offset || data[lfh..lfh + 4] != [0x50, 0x4b, 0x03, 0x04] {
            return Err(PackageError::InvalidArchive(format!(
                "invalid local file header for `{}`",
                rec.name
            )));
        }
        let lfh_flags = le_u16(data, lfh + 6)?;
        let lfh_method = le_u16(data, lfh + 8)?;
        let lfh_crc32 = le_u32(data, lfh + 14)?;
        let lfh_comp_size = le_u32(data, lfh + 18)?;
        let lfh_uncomp_size = le_u32(data, lfh + 22)?;
        let lfh_name_len = le_u16(data, lfh + 26)? as usize;
        let lfh_extra_len = le_u16(data, lfh + 28)? as usize;
        let name_start = lfh + 30;
        let extra_start = name_start + lfh_name_len;
        let data_start = extra_start + lfh_extra_len;
        let data_end = data_start
            .checked_add(rec.compressed_size as usize)
            .filter(|&end| end <= cd_offset)
            .ok_or_else(|| {
                PackageError::InvalidArchive("entry compressed data out of bounds".into())
            })?;
        if lfh_method != rec.method || lfh_flags != rec.flags {
            return Err(PackageError::InvalidArchive(format!(
                "local/central header mismatch for `{}`",
                rec.name
            )));
        }
        if (rec.flags & 0x0008) == 0
            && (lfh_crc32 != rec.crc32
                || lfh_comp_size != rec.compressed_size
                || lfh_uncomp_size != rec.uncompressed_size)
        {
            return Err(PackageError::InvalidArchive(format!(
                "local/central size or crc mismatch for `{}`",
                rec.name
            )));
        }
        if &data[name_start..extra_start] != rec.name.as_bytes() {
            return Err(PackageError::InvalidArchive(format!(
                "local/central filename mismatch for `{}`",
                rec.name
            )));
        }
        if has_winzip_aes_extra(&data[extra_start..data_start]) {
            return Err(PackageError::EncryptedArchiveEntry(rec.name.clone()));
        }
        spans.push((lfh, data_end, (rec.flags & 0x0008) != 0));
    }
    spans.sort_unstable_by_key(|s| s.0);
    if spans[0].0 != 0 {
        return Err(PackageError::InvalidArchive(
            "unexpected prepended bytes before first local file header".into(),
        ));
    }
    for i in 0..spans.len() {
        let (_, end, has_dd) = spans[i];
        let next_start = if i + 1 < spans.len() {
            spans[i + 1].0
        } else {
            cd_offset
        };
        if next_start < end {
            return Err(PackageError::InvalidArchive(
                "overlapping local file entries in ZIP".into(),
            ));
        }
        let gap = next_start - end;
        let valid_gap = gap == 0 || (has_dd && matches!(gap, 12 | 16));
        if !valid_gap {
            return Err(PackageError::InvalidArchive(
                "unexpected gap between ZIP local file entries".into(),
            ));
        }
    }
    Ok(())
}

fn check_entry_security(
    name: &str,
    flags: u16,
    method: u16,
    external_attr: u32,
    extra: &[u8],
) -> Result<(), PackageError> {
    if (flags & 0x0001) != 0 || (flags & 0x0040) != 0 || has_winzip_aes_extra(extra) {
        return Err(PackageError::EncryptedArchiveEntry(name.to_string()));
    }
    if method != 0 && method != 8 {
        return Err(PackageError::UnsupportedCompression {
            path: name.to_string(),
            method,
        });
    }
    let is_dir = name.ends_with('/');
    let trimmed = if is_dir {
        name.strip_suffix('/').unwrap_or(name)
    } else {
        name
    };
    validate_safe_rel_path(trimmed, name)?;

    let unix_mode = external_attr >> 16;
    let file_type = unix_mode & 0o170000;
    let dos_attr = external_attr & 0xFF;
    let has_dos_dir = (dos_attr & 0x10) != 0;
    if is_dir {
        if file_type != 0 && file_type != 0o040000 {
            return Err(PackageError::ForbiddenEntryType {
                path: name.to_string(),
                reason: format!("non-directory mode {file_type:#o} on directory entry"),
            });
        }
        if (dos_attr != 0 && !has_dos_dir) || (file_type == 0 && unix_mode != 0 && !has_dos_dir) {
            return Err(PackageError::ForbiddenEntryType {
                path: name.to_string(),
                reason: "directory entry lacks directory mode/attribute".into(),
            });
        }
    } else {
        if file_type != 0 && file_type != 0o100000 {
            return Err(PackageError::ForbiddenEntryType {
                path: name.to_string(),
                reason: format!("non-regular file mode {file_type:#o}"),
            });
        }
        if has_dos_dir {
            return Err(PackageError::ForbiddenEntryType {
                path: name.to_string(),
                reason: "DOS directory attribute set on non-directory entry".into(),
            });
        }
        if (unix_mode & 0o7111) != 0 {
            return Err(PackageError::ForbiddenExecutable(format!(
                "{name} (unix executable or special mode bits)"
            )));
        }
        if let Some(ext) = trimmed.rsplit('.').next()
            && trimmed.contains('.')
        {
            let lower = ext.to_ascii_lowercase();
            if FORBIDDEN_EXEC_EXTS.contains(&lower.as_str()) {
                return Err(PackageError::ForbiddenExecutable(name.to_string()));
            }
        }
    }
    Ok(())
}

fn fold_path(path: &str) -> String {
    path.nfc().collect::<String>().to_lowercase()
}

fn extract_all_entries(
    data: &[u8],
    records: &[CdRecord],
    limits: &ArchiveLimits,
) -> Result<BTreeMap<String, ExtractedEntry>, PackageError> {
    let mut exact_seen = HashSet::new();
    let mut collision_seen: HashMap<String, String> = HashMap::new();
    let mut explicit_dirs = HashSet::new();
    let mut implied_dirs = HashSet::new();
    let mut files = BTreeMap::new();
    let mut total_uncompressed: u64 = 0;
    let mut total_compressed: u64 = 0;

    for rec in records {
        let canonical_key = rec.name.trim_end_matches('/');
        if !exact_seen.insert(rec.name.clone()) {
            return Err(PackageError::DuplicatePath(rec.name.clone()));
        }
        let fold_key = fold_path(canonical_key);
        if let Some(existing) = collision_seen.get(&fold_key) {
            return Err(PackageError::PathCollision {
                path: rec.name.clone(),
                existing: existing.clone(),
            });
        }
        collision_seen.insert(fold_key.clone(), rec.name.clone());

        record_parent_dirs(canonical_key, &mut implied_dirs);
        if rec.is_dir {
            if rec.compressed_size != 0 || rec.uncompressed_size != 0 {
                return Err(PackageError::InvalidArchive(format!(
                    "directory entry `{}` has non-zero payload",
                    rec.name
                )));
            }
            explicit_dirs.insert(fold_key);
            continue;
        }

        let extracted =
            extract_single_entry(data, rec, limits, total_uncompressed, total_compressed)?;
        total_uncompressed = total_uncompressed
            .checked_add(extracted.size)
            .ok_or_else(|| PackageError::LimitExceeded("total uncompressed overflow".into()))?;
        total_compressed = total_compressed.saturating_add(rec.compressed_size as u64);
        files.insert(rec.name.clone(), extracted);
    }

    for path in files.keys() {
        let fold = fold_path(path);
        if explicit_dirs.contains(&fold) || implied_dirs.contains(&fold) {
            return Err(PackageError::InvalidArchive(format!(
                "path `{path}` conflicts with directory entry"
            )));
        }
    }
    Ok(files)
}

fn extract_single_entry(
    data: &[u8],
    rec: &CdRecord,
    limits: &ArchiveLimits,
    total_uncompressed_so_far: u64,
    total_compressed_so_far: u64,
) -> Result<ExtractedEntry, PackageError> {
    let lfh = rec.lfh_offset;
    let lfh_name_len = le_u16(data, lfh + 26)? as usize;
    let lfh_extra_len = le_u16(data, lfh + 28)? as usize;
    let data_start = lfh + 30 + lfh_name_len + lfh_extra_len;
    let data_end = data_start + rec.compressed_size as usize;

    let compressed = &data[data_start..data_end];
    let out = stream_decompress(
        compressed,
        rec,
        limits,
        total_uncompressed_so_far,
        total_compressed_so_far,
    )?;
    let sha = Sha256::digest(&out);
    Ok(ExtractedEntry {
        path: rec.name.clone(),
        size: out.len() as u64,
        sha256_hex: hex_lower(sha.as_slice()),
        bytes: out,
    })
}

fn stream_decompress(
    compressed: &[u8],
    rec: &CdRecord,
    limits: &ArchiveLimits,
    total_uncompressed_so_far: u64,
    total_compressed_so_far: u64,
) -> Result<Vec<u8>, PackageError> {
    let mut crc = Crc32::new();
    let out = match rec.method {
        0 => read_bounded_entry_stream(
            compressed,
            rec,
            limits,
            total_uncompressed_so_far,
            total_compressed_so_far,
            &mut crc,
        )?,
        8 => {
            let mut dec = DeflateDecoder::new(compressed);
            let out = read_bounded_entry_stream(
                &mut dec,
                rec,
                limits,
                total_uncompressed_so_far,
                total_compressed_so_far,
                &mut crc,
            )?;
            if dec.total_in() != rec.compressed_size as u64 {
                return Err(PackageError::InvalidArchive(format!(
                    "compressed payload size mismatch / trailing padding for `{}`",
                    rec.name
                )));
            }
            out
        }
        method => {
            return Err(PackageError::UnsupportedCompression {
                path: rec.name.clone(),
                method,
            });
        }
    };

    if crc.finalize() != rec.crc32 {
        return Err(PackageError::InvalidArchive(format!(
            "CRC-32 mismatch for `{}`",
            rec.name
        )));
    }
    if out.len() as u64 != rec.uncompressed_size as u64 {
        return Err(PackageError::InvalidArchive(format!(
            "uncompressed size mismatch for `{}`",
            rec.name
        )));
    }
    Ok(out)
}

fn read_bounded_entry_stream<R: Read>(
    mut reader: R,
    rec: &CdRecord,
    limits: &ArchiveLimits,
    total_uncompressed_so_far: u64,
    total_compressed_so_far: u64,
    crc: &mut Crc32,
) -> Result<Vec<u8>, PackageError> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    let compressed_denom = (rec.compressed_size as u64).max(1);
    let agg_compressed_denom = total_compressed_so_far
        .saturating_add(rec.compressed_size as u64)
        .max(1);

    loop {
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(PackageError::InvalidArchive(format!(
                    "decompression error on `{}`: {e}",
                    rec.name
                )));
            }
        };
        if n == 0 {
            break;
        }
        let next_entry_size = (out.len() as u64).saturating_add(n as u64);
        if next_entry_size > limits.max_entry_uncompressed_bytes {
            return Err(PackageError::LimitExceeded(format!(
                "entry `{}` exceeds max uncompressed size {}",
                rec.name, limits.max_entry_uncompressed_bytes
            )));
        }
        let agg_uncompressed = total_uncompressed_so_far.saturating_add(next_entry_size);
        if agg_uncompressed > limits.max_total_uncompressed_bytes {
            return Err(PackageError::LimitExceeded(format!(
                "archive total uncompressed size exceeds {}",
                limits.max_total_uncompressed_bytes
            )));
        }
        if next_entry_size > limits.ratio_check_min_bytes
            && next_entry_size / compressed_denom > limits.max_compression_ratio
        {
            return Err(PackageError::CompressionRatioExceeded(format!(
                "entry `{}` exceeds max compression ratio {}",
                rec.name, limits.max_compression_ratio
            )));
        }
        if agg_uncompressed > limits.ratio_check_min_bytes
            && agg_uncompressed / agg_compressed_denom > limits.max_compression_ratio
        {
            return Err(PackageError::CompressionRatioExceeded(format!(
                "archive aggregate compression ratio exceeds {}",
                limits.max_compression_ratio
            )));
        }
        crc.update(&buf[..n]);
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

pub(crate) fn validate_safe_rel_path(path: &str, display_path: &str) -> Result<(), PackageError> {
    if path.is_empty() {
        return Err(PackageError::UnsafePath {
            path: display_path.to_string(),
            reason: "empty path".into(),
        });
    }
    if path.starts_with('/') {
        return Err(PackageError::UnsafePath {
            path: display_path.to_string(),
            reason: "absolute path".into(),
        });
    }
    if path.contains('\\') {
        return Err(PackageError::UnsafePath {
            path: display_path.to_string(),
            reason: "backslash separator".into(),
        });
    }
    if path.chars().any(|c| c == '\0' || c.is_control()) {
        return Err(PackageError::UnsafePath {
            path: display_path.to_string(),
            reason: "control or NUL character".into(),
        });
    }
    if path.contains('?') || path.contains('#') {
        return Err(PackageError::UnsafePath {
            path: display_path.to_string(),
            reason: "query or fragment delimiter".into(),
        });
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(PackageError::UnsafePath {
            path: display_path.to_string(),
            reason: "Windows drive prefix".into(),
        });
    }
    for seg in path.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(PackageError::UnsafePath {
                path: display_path.to_string(),
                reason: "invalid path segment".into(),
            });
        }
    }
    Ok(())
}

fn has_winzip_aes_extra(extra: &[u8]) -> bool {
    let mut i = 0;
    while i + 4 <= extra.len() {
        let id = u16::from_le_bytes([extra[i], extra[i + 1]]);
        let len = u16::from_le_bytes([extra[i + 2], extra[i + 3]]) as usize;
        if id == 0x9901 {
            return true;
        }
        i = i.saturating_add(4 + len);
    }
    false
}

fn record_parent_dirs(path: &str, dirs: &mut HashSet<String>) {
    let mut cur = path;
    while let Some((parent, _)) = cur.rsplit_once('/') {
        if !parent.is_empty() {
            dirs.insert(fold_path(parent));
        }
        cur = parent;
    }
}

fn le_u16(data: &[u8], off: usize) -> Result<u16, PackageError> {
    let slice = data
        .get(off..off + 2)
        .ok_or_else(|| PackageError::InvalidArchive("truncated ZIP header".into()))?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn le_u32(data: &[u8], off: usize) -> Result<u32, PackageError> {
    let slice = data
        .get(off..off + 4)
        .ok_or_else(|| PackageError::InvalidArchive("truncated ZIP header".into()))?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}
