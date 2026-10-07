use std::path::Path;
use std::sync::Arc;

use crate::catalog::ArtworkCandidate;
use domain::Media;

use crate::http_agent;
use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;

pub trait PosterFetch: Send + Sync {
    fn get(&self, url: &str) -> Result<Vec<u8>, String>;
}

pub struct HttpPoster;

impl PosterFetch for HttpPoster {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        http_agent::call_url(url, |agent| agent.get(url).call())
            .map_err(|err| err.to_string())?
            .into_body()
            .read_to_vec()
            .map_err(|err| err.to_string())
    }
}

/// Best-effort poster for a transferred file (Emby-style):
/// 1. catalog poster URL (TMDB etc.) if reachable;
/// 2. else extract a video frame locally (network-independent fallback).
/// Writes `poster.jpg` beside the video. Any failure keeps sidecars intact.
pub fn attach_poster(state: &ApiState, media: &Media, video: &Path) -> Option<Vec<u8>> {
    // 刮削与整理设置：写入图片总开关（mirror_images）。
    let mirror_images = state
        .store
        .lock()
        .get_scrape_config()
        .ok()
        .map(|config| config.effective.mirror_images)
        .unwrap_or(true);
    if !mirror_images {
        return None;
    }
    let dir = video.parent()?;
    let target = dir.join("poster.jpg");
    tracing::debug!(media = %media.title, video = %video.display(), "为媒体文件补充海报");

    // A user-selected or uploaded cover is authoritative. Metadata refreshes
    // and future imports must not overwrite it (Emby-style artwork lock).
    let locked = state
        .store
        .lock()
        .get_setting(&format!("artwork.{}", media.id))
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|value| value["poster_locked"].as_bool())
        .unwrap_or(false);
    if locked && target.is_file() {
        return std::fs::read(&target).ok();
    }

    // Channel 1: catalog artwork. poster_mode=language picks per the language
    // priority + min-width gate; default keeps the TMDB detail poster. Both
    // paths honor poster_size so sidecars are suitable for full-screen views.
    if let Some(tmdb_id) = media.tmdb_id.as_deref() {
        let config = state.store.lock().get_scrape_config().ok();
        let size = config
            .as_ref()
            .map(|c| c.effective.poster_size.as_str())
            .unwrap_or("w780");
        let picked: Option<Vec<u8>> =
            match config.as_ref().map(|c| c.effective.poster_mode.as_str()) {
                Some("language") => {
                    let candidates = state.catalog.image_candidates(media.kind, tmdb_id).ok();
                    if let Some(candidates) = candidates {
                        let effective = config.as_ref().map(|c| &c.effective);
                        let meta = effective.map(|e| e.primary_language()).unwrap_or("zh-CN");
                        let priority = effective
                            .map(|e| e.poster_language_priority.as_slice())
                            .unwrap_or_default();
                        let min = effective.map(|e| e.poster_min_width).unwrap_or(500);
                        let size = effective.map(|e| e.poster_size.as_str()).unwrap_or("w780");
                        pick_candidate(&candidates.posters, priority, meta, min)
                            .and_then(|c| state.poster_fetch.get(&c.url(size)).ok())
                            .filter(|bytes| !bytes.is_empty())
                    } else {
                        None
                    }
                }
                _ => state
                    .catalog
                    .poster_url(media.kind, tmdb_id)
                    .ok()
                    .flatten()
                    .map(|url| poster_url_at_size(&url, size))
                    .and_then(|url| state.poster_fetch.get(&url).ok())
                    .filter(|bytes| !bytes.is_empty()),
            };
        if let Some(bytes) = picked {
            std::fs::write(&target, &bytes).ok()?;
            return Some(bytes);
        }
    }

    // Channel 2: local video frame → 2:3 poster (no network needed).
    // 对于 STRM 虚拟流媒体文件，跳过本地强制抓帧（避免 ffmpeg 读纯文本报错），
    // 且避免对远程挂载发起大流量视频流拉取
    let is_strm = video
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("strm"));
    if !is_strm {
        let generate_allowed = {
            let store = state.store.lock();
            if let Ok(libraries) = store.list_libraries() {
                libraries
                    .into_iter()
                    .filter(|lib| lib.root_paths.iter().any(|r| video.starts_with(r)))
                    .max_by_key(|lib| {
                        lib.root_paths
                            .iter()
                            .filter(|r| video.starts_with(r))
                            .map(|r| r.components().count())
                            .max()
                            .unwrap_or(0)
                    })
                    .map(|lib| lib.generate_thumbnails)
                    .unwrap_or(true)
            } else {
                true
            }
        };
        if generate_allowed {
            if let Ok(bytes) = frame_poster(video) {
                if !bytes.is_empty() {
                    std::fs::write(&target, &bytes).ok()?;
                    return Some(bytes);
                }
            }
        }
    }
    None
}

fn poster_url_at_size(url: &str, size: &str) -> String {
    url.replace("/t/p/w342/", &format!("/t/p/{size}/"))
}

/// Extract a 2:3 poster (342x513) from a video frame via ffmpeg.
pub fn frame_poster(video: &Path) -> Result<Vec<u8>, String> {
    use std::process::Command;
    let out = tempfile_posters_dir()?.join(format!(
        "frame-{}-{}.jpg",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let status = Command::new("ffmpeg")
        .args([
            "-y", "-ss", "60", "-i", &video.display().to_string(),
            "-frames:v", "1",
            "-vf", "scale=342:513:force_original_aspect_ratio=decrease,pad=342:513:(ow-iw)/2:(oh-ih)/2:black",
            "-q:v", "2", &out.display().to_string(),
        ])
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("ffmpeg frame extraction failed".into());
    }
    let bytes = std::fs::read(&out).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&out);
    Ok(bytes)
}

fn tempfile_posters_dir() -> Result<std::path::PathBuf, String> {
    let dir = std::env::temp_dir().join("crawler-media-posters");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

pub fn poster_fetch(state: &ApiState) -> Arc<dyn PosterFetch> {
    state.poster_fetch.clone()
}

/// Pick an artwork candidate by language-priority tiers + min-width gate.
/// Priority items: "" = 无文字 (null language), "meta" = metadata primary
/// language, otherwise a language code (subtag match). Falls back to any
/// candidate above the gate, then the first candidate.
pub fn pick_candidate<'a>(
    candidates: &'a [ArtworkCandidate],
    priority: &[String],
    meta_language: &str,
    min_width: u32,
) -> Option<&'a ArtworkCandidate> {
    let meta = lang_subtag(meta_language);
    for item in priority {
        let want = match item.trim() {
            "" => Some(None),
            "meta" => Some(Some(meta.clone())),
            lang => Some(Some(lang_subtag(lang))),
        };
        let Some(want) = want else { continue };
        if let Some(candidate) = candidates
            .iter()
            .find(|c| c.width >= min_width && lang_matches(&c.language, &want))
        {
            return Some(candidate);
        }
    }
    candidates
        .iter()
        .find(|c| c.width >= min_width)
        .or_else(|| candidates.first())
}

/// Best-effort backdrop (fanart.jpg) beside a file, using the backdrop
/// language priority + min-width gate. Any failure keeps sidecars intact.
pub fn attach_backdrop(state: &ApiState, media: &Media, video: &Path) -> Option<Vec<u8>> {
    attach_backdrop_opts(state, media, video, false)
}

pub fn force_attach_backdrop(state: &ApiState, media: &Media, video: &Path) -> Option<Vec<u8>> {
    attach_backdrop_opts(state, media, video, true)
}

fn attach_backdrop_opts(
    state: &ApiState,
    media: &Media,
    video: &Path,
    force: bool,
) -> Option<Vec<u8>> {
    let config = state.store.lock().get_scrape_config().ok()?;
    if !config.effective.mirror_images {
        return None;
    }
    let dir = video.parent()?;
    let show_dir = if dir
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().starts_with("season"))
    {
        dir.parent().unwrap_or(dir)
    } else {
        dir
    };
    let target = show_dir.join("fanart.jpg");
    if !force && target.is_file() {
        return std::fs::read(&target).ok();
    }
    let tmdb_id = media.tmdb_id.as_deref()?;
    let candidates = state.catalog.image_candidates(media.kind, tmdb_id).ok()?;
    if candidates.backdrops.is_empty() {
        return None;
    }
    let picked = pick_candidate(
        &candidates.backdrops,
        &config.effective.backdrop_language_priority,
        config.effective.primary_language(),
        config.effective.backdrop_min_width,
    )?;
    let bytes = state
        .poster_fetch
        .get(&picked.url(&config.effective.backdrop_size))
        .ok()?;
    if bytes.is_empty() {
        return None;
    }
    std::fs::write(&target, &bytes).ok()?;
    Some(bytes)
}

fn lang_subtag(language: &str) -> String {
    language
        .split('-')
        .next()
        .unwrap_or(language)
        .to_lowercase()
}

fn lang_matches(candidate: &Option<String>, want: &Option<String>) -> bool {
    match (candidate, want) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        (None, None) => true,
        _ => false,
    }
}
