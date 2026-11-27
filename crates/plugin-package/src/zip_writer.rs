use crate::archive::validate_safe_rel_path;
use crate::{ArchiveLimits, PackageError};
use flate2::Compression;
use flate2::write::DeflateEncoder;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use unicode_normalization::UnicodeNormalization;

pub fn validate_safe_package_path(path: &str) -> Result<(), PackageError> {
    validate_safe_rel_path(path, path)
}

pub fn build_deterministic_cmplugin_zip(
    entries: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<u8>, PackageError> {
    let limits = ArchiveLimits::default();
    if entries.len() > limits.max_entries {
        return Err(PackageError::LimitExceeded(format!(
            "archive entry count {} exceeds limit {}",
            entries.len(),
            limits.max_entries
        )));
    }
    if entries.len() >= u16::MAX as usize {
        return Err(PackageError::LimitExceeded(
            "archive entry count exceeds non-ZIP64 limit".into(),
        ));
    }

    let mut seen_fold: HashMap<String, String> = HashMap::with_capacity(entries.len());
    let mut total_uncompressed: u64 = 0;

    for (path, bytes) in entries {
        validate_safe_rel_path(path, path)?;
        let fold: String = path.nfc().collect::<String>().to_lowercase();
        if let Some(existing) = seen_fold.get(&fold) {
            return Err(PackageError::PathCollision {
                path: path.clone(),
                existing: existing.clone(),
            });
        }
        seen_fold.insert(fold, path.clone());

        let len_u64 = bytes.len() as u64;
        if len_u64 > limits.max_entry_uncompressed_bytes {
            return Err(PackageError::LimitExceeded(format!(
                "entry `{path}` exceeds max uncompressed size {}",
                limits.max_entry_uncompressed_bytes
            )));
        }
        total_uncompressed = total_uncompressed
            .checked_add(len_u64)
            .ok_or_else(|| PackageError::LimitExceeded("archive size overflow".into()))?;
        if total_uncompressed > limits.max_total_uncompressed_bytes {
            return Err(PackageError::LimitExceeded(format!(
                "archive total uncompressed size exceeds {}",
                limits.max_total_uncompressed_bytes
            )));
        }
    }

    let mut out = Vec::new();
    let mut cd = Vec::new();

    for (path, bytes) in entries {
        write_zip_entry(path, bytes, &limits, &mut out, &mut cd)?;
    }

    let cd_offset = u32::try_from(out.len()).map_err(|_| {
        PackageError::LimitExceeded("archive exceeds 4 GiB non-ZIP64 offset".into())
    })?;
    let cd_size = u32::try_from(cd.len()).map_err(|_| {
        PackageError::LimitExceeded("central directory exceeds non-ZIP64 size".into())
    })?;
    out.extend_from_slice(&cd);

    let count_u16 = entries.len() as u16;
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&count_u16.to_le_bytes());
    out.extend_from_slice(&count_u16.to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());

    if (out.len() as u64) > limits.max_package_bytes {
        return Err(PackageError::LimitExceeded(format!(
            "compressed package exceeds {} bytes",
            limits.max_package_bytes
        )));
    }
    Ok(out)
}

fn write_zip_entry(
    path: &str,
    bytes: &[u8],
    limits: &ArchiveLimits,
    out: &mut Vec<u8>,
    cd: &mut Vec<u8>,
) -> Result<(), PackageError> {
    let name_bytes = path.as_bytes();
    let name_len = u16::try_from(name_bytes.len())
        .map_err(|_| PackageError::InvalidArchive(format!("path `{path}` exceeds 65535 bytes")))?;
    let uncompressed_size = u32::try_from(bytes.len())
        .map_err(|_| PackageError::LimitExceeded(format!("entry `{path}` exceeds 4 GiB")))?;
    let crc32 = crc32fast::hash(bytes);

    let (method, payload) = choose_compression(bytes, limits)?;
    let compressed_size = u32::try_from(payload.len()).map_err(|_| {
        PackageError::LimitExceeded(format!("entry `{path}` compressed size overflow"))
    })?;
    let lfh_offset = u32::try_from(out.len()).map_err(|_| {
        PackageError::LimitExceeded("archive offset exceeds non-ZIP64 limit".into())
    })?;

    let version_needed: u16 = 20;
    let version_made_by: u16 = (3 << 8) | 20;
    let flags: u16 = 0x0800; // UTF-8 filename flag
    let external_attr: u32 = 0o100644 << 16;

    out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
    out.extend_from_slice(&version_needed.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&method.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0x0021u16.to_le_bytes());
    out.extend_from_slice(&crc32.to_le_bytes());
    out.extend_from_slice(&compressed_size.to_le_bytes());
    out.extend_from_slice(&uncompressed_size.to_le_bytes());
    out.extend_from_slice(&name_len.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(name_bytes);
    out.extend_from_slice(&payload);

    cd.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
    cd.extend_from_slice(&version_made_by.to_le_bytes());
    cd.extend_from_slice(&version_needed.to_le_bytes());
    cd.extend_from_slice(&flags.to_le_bytes());
    cd.extend_from_slice(&method.to_le_bytes());
    cd.extend_from_slice(&0u16.to_le_bytes());
    cd.extend_from_slice(&0x0021u16.to_le_bytes());
    cd.extend_from_slice(&crc32.to_le_bytes());
    cd.extend_from_slice(&compressed_size.to_le_bytes());
    cd.extend_from_slice(&uncompressed_size.to_le_bytes());
    cd.extend_from_slice(&name_len.to_le_bytes());
    cd.extend_from_slice(&0u16.to_le_bytes());
    cd.extend_from_slice(&0u16.to_le_bytes());
    cd.extend_from_slice(&0u16.to_le_bytes());
    cd.extend_from_slice(&0u16.to_le_bytes());
    cd.extend_from_slice(&external_attr.to_le_bytes());
    cd.extend_from_slice(&lfh_offset.to_le_bytes());
    cd.extend_from_slice(name_bytes);
    Ok(())
}

fn choose_compression(
    bytes: &[u8],
    limits: &ArchiveLimits,
) -> Result<(u16, Vec<u8>), PackageError> {
    if bytes.is_empty() {
        return Ok((0, Vec::new()));
    }
    let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
    enc.write_all(bytes)
        .map_err(|e| PackageError::InvalidArchive(format!("deflate write failed: {e}")))?;
    let deflated = enc
        .finish()
        .map_err(|e| PackageError::InvalidArchive(format!("deflate finish failed: {e}")))?;
    let denom = (deflated.len() as u64).max(1);
    let within_ratio = (bytes.len() as u64) <= limits.ratio_check_min_bytes
        || (bytes.len() as u64) / denom < limits.max_compression_ratio;
    if deflated.len() < bytes.len() && within_ratio {
        Ok((8, deflated))
    } else {
        Ok((0, bytes.to_vec()))
    }
}
