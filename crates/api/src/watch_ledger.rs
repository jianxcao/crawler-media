use std::path::PathBuf;

use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, Release, UnitKey};
use store::ScannedLedger;

use crate::Store;

pub fn record_paths(
    store: &Store,
    paths: impl IntoIterator<Item = library::TransferredFile>,
) -> Result<Vec<PathBuf>, String> {
    let mut inserted = Vec::new();
    let mut rows: Vec<ScannedLedger> = Vec::new();
    let now_ms = crate::job_loop::unix_now() * 1000;
    for file in paths {
        let dest = file.path;
        let path_str = dest.display().to_string();
        // 该路径已有台账行（Transfer 时 probe 过、带 filter_score）：扫描
        // 绝不能按文件名覆盖更可靠的既有信息。复用已有行，仅保留。
        if store
            .ledger_by_path(&path_str)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            match store.clear_ledger_missing(&path_str) {
                Ok(true) => inserted.push(dest),
                Ok(false) => {}
                Err(error) => {
                    tracing::error!(
                        %error,
                        path = %path_str,
                        "清除台账缺失标记失败：该路径不会进入本次新增/探测队列"
                    );
                }
            }
            continue;
        }
        let name = dest
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        // B14: 优先使用 scan 阶段经过 NFO/父目录 fallback 识别的 release，
        // 只有未预识别时才按 basename 重新解析。
        let parsed = file
            .identified_release
            .unwrap_or_else(|| release::parse(name));
        let mut incoming = media_from_release(&parsed);
        if let Some(identity) = file.nfo_identity {
            incoming.kind = identity.kind;
            incoming.title = identity.title.unwrap_or(incoming.title);
            incoming.year = identity.year;
            incoming.original_title = identity.original_title;
            incoming.tmdb_id = identity.tmdb_id;
            incoming.douban_id = identity.douban_id;
            incoming.tvdb_id = identity.tvdb_id;
            incoming.bangumi_id = identity.bangumi_id;
            incoming.anilist_id = identity.anilist_id;
        }
        let media = store
            .resolve_scanned_media(incoming)
            .map_err(|e| e.to_string())?;
        let units = match media.kind {
            MediaKind::Tv => parsed
                .covered_episodes()
                .into_iter()
                .map(|(season, episode)| UnitKey::new(season as i32, episode as i32))
                .collect(),
            _ => vec![UnitKey::WHOLE],
        };
        let (size_bytes, created_ns, modified_ns) = observed_stat(&dest);
        let source_version = crate::fingerprint_job::current_source_version(&dest);
        // The sidecar is the scan's own observation input: read it here, before the
        // publication transaction, and hand the result to the writer.
        let observation = store
            .library_for_path_strict(&dest, media.kind)
            .map_err(|e| e.to_string())?
            .map(|library| {
                let locale = library
                    .id
                    .parse()
                    .ok()
                    .and_then(|library_id| {
                        store::library_query::library_display_locale(store, library_id)
                            .ok()
                            .flatten()
                    })
                    .unwrap_or_default();
                let config_revision = store
                    .library_config_revision(&library.id)
                    .unwrap_or(0);
                store::metadata::NfoObservation::observe(
                    &dest,
                    media.kind,
                    locale,
                    source_version.clone(),
                    config_revision,
                )
            });
        rows.push(ScannedLedger {
            row: LedgerRow {
                id: LedgerId::new(), media_id: media.id, path: path_str,
                season: parsed.season, episode: parsed.episode,
                resolution: parsed.resolution, codec: parsed.codec, hdr: parsed.hdr,
                quality_source: QualitySource::Release, confidence: parsed.confidence,
                filter_score: None,
            },
            units,
            source_version,
            source_digest: None,
            size_bytes,
            created_ns,
            modified_ns,
            observation,
        });
        inserted.push(dest);
    }
    publish_scanned_rows(store, &rows, now_ms)?;
    Ok(inserted)
}

/// 「其他」库扫描：每个视频文件一个 Media（kind=video，标题 = 文件名主干，
/// 不做任何识别/刮削）。
pub fn record_video_paths(
    store: &Store,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<Vec<PathBuf>, String> {
    let mut inserted = Vec::new();
    let mut rows: Vec<ScannedLedger> = Vec::new();
    let now_ms = crate::job_loop::unix_now() * 1000;
    for dest in paths {
        let path_str = dest.display().to_string();
        // 已有台账行：复用既有 Media ID（播放进度/收藏挂在它上面），
        // 绝不再建一个无别名的新 Media 顶替（P2：重扫换 Media ID）。
        if store
            .ledger_by_path(&path_str)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            match store.clear_ledger_missing(&path_str) {
                Ok(true) => inserted.push(dest),
                Ok(false) => {}
                Err(error) => {
                    tracing::error!(
                        %error,
                        path = %path_str,
                        "清除台账缺失标记失败：该路径不会进入本次新增/探测队列"
                    );
                }
            }
            continue;
        }
        let stem = dest
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let media = store
            .ensure_media(Media {
                id: MediaId::new(),
                kind: MediaKind::Video,
                title: stem,
                year: None,
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .map_err(|e| e.to_string())?;
        let (size_bytes, created_ns, modified_ns) = observed_stat(&dest);
        rows.push(ScannedLedger {
            row: LedgerRow {
                id: LedgerId::new(),
                media_id: media.id,
                path: dest.display().to_string(),
                season: None,
                episode: None,
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
            },
            units: vec![UnitKey::WHOLE],
            source_version: crate::fingerprint_job::current_source_version(&dest),
            source_digest: None,
            size_bytes,
            created_ns,
            modified_ns,
            observation: None,
        });
        inserted.push(dest);
    }
    publish_scanned_rows(store, &rows, now_ms)?;
    Ok(inserted)
}

/// Commit scanned ledger rows in the scan commit window: at most
/// [`store::MAX_SCAN_BATCH`] files per transaction, so one media is reduced once per
/// window instead of once per file.
fn publish_scanned_rows(store: &Store, rows: &[ScannedLedger], now_ms: i64) -> Result<(), String> {
    for batch in rows.chunks(store::MAX_SCAN_BATCH) {
        store
            .insert_scanned_files(batch, now_ms)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Stat facts captured while the scan still holds the observed path. `None` means the
/// observation failed (permissions, race): the row keeps `stat_known = 0` instead of
/// inventing zero-valued metadata.
fn observed_stat(path: &std::path::Path) -> (Option<i64>, Option<i64>, Option<i64>) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return (None, None, None);
    };
    let to_ns = |time: std::io::Result<std::time::SystemTime>| {
        time.ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|age| i64::try_from(age.as_nanos()).ok())
    };
    (
        i64::try_from(metadata.len()).ok(),
        to_ns(metadata.created()),
        to_ns(metadata.modified()),
    )
}

fn media_from_release(parsed: &Release) -> Media {
    Media {
        id: MediaId::new(),
        kind: if parsed.season.is_some() {
            MediaKind::Tv
        } else {
            MediaKind::Movie
        },
        title: parsed.title.clone(),
        year: parsed.year,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}
