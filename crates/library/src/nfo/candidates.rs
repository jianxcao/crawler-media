//! NFO sidecar 候选与作用域判定。
//!
//! 一个视频文件旁边的 NFO 可能描述三层实体中的任意一层：整部 Work（`tvshow.nfo` /
//! `movie.nfo`）、某一季（`season.nfo`）、某一集（与视频同名的 `.nfo`）。候选顺序与
//! 旧实现保持一致，但每一份候选都带上自己的 [`NfoLayer`]，并把"这份文件到底属于谁"
//! 交给 [`resolve_nfo_scope`] 判定：只有在根元素类型、目录名和台账覆盖三者一致时才归位，
//! 否则返回 `None`（调用方按 Unknown 处理，保留上一份有效输入）。
//!
//! 不变量（勿"顺手修回"）：坐标无法与已知覆盖对上时**绝不回落到 Work**。
//! 回落会让一份 `<episodedetails>` 冒充整部剧的元数据，把正确的 Work 层输入覆盖掉；
//! 宁可返回 `None`（Unknown，上层不采纳也不删除别人的输入），也不要猜一层出来。

use std::path::{Path, PathBuf};

use domain::{EntityScope, MediaKind, UnitKey};

use super::parse::NfoHeader;

/// 一份 sidecar 在层级结构中的位置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NfoLayer {
    /// 整部作品：`tvshow.nfo` / `movie.nfo` / `<video>` 同层。
    Work,
    /// 某一季：`season.nfo`。
    Season,
    /// 某一集：与视频文件同名的 `.nfo`。
    Episode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NfoSidecar {
    pub path: PathBuf,
    pub layer: NfoLayer,
}

/// 与视频同目录（TV 还会向上找到剧集根目录）的 NFO 候选，按优先级排序并去重。
pub fn nfo_sidecars(path: &Path, kind: MediaKind) -> Vec<NfoSidecar> {
    let directory = path.parent().unwrap_or(path);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let candidates = match kind {
        MediaKind::Tv => {
            let season_dir = directory
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.to_ascii_lowercase().starts_with("season"));
            let series = if season_dir {
                directory.parent().unwrap_or(directory)
            } else {
                directory
            };
            vec![
                (path.with_extension("nfo"), NfoLayer::Episode),
                (directory.join("season.nfo"), NfoLayer::Season),
                (series.join("tvshow.nfo"), NfoLayer::Work),
            ]
        }
        _ => vec![
            (path.with_extension("nfo"), NfoLayer::Work),
            (directory.join(format!("{stem}.nfo")), NfoLayer::Work),
            (directory.join("movie.nfo"), NfoLayer::Work),
        ],
    };
    let mut sidecars: Vec<NfoSidecar> = Vec::with_capacity(candidates.len());
    for (candidate, layer) in candidates {
        if sidecars.iter().any(|known| known.path == candidate) {
            continue;
        }
        sidecars.push(NfoSidecar {
            path: candidate,
            layer,
        });
    }
    sidecars
}

/// 由 sidecar 所在层级、视频路径与台账覆盖判定它描述的实体。
///
/// - `Work`：根元素必须是作品级根（`movie`/`tvshow`/`video`/…）。
/// - `Season`：根元素必须是 `season`；季号取自目录名（`Season 2`），目录名没有季号时回落到
///   XML `<season>`，与已知覆盖的季号冲突则拒绝。
/// - `Episode`：优先 XML 的季集坐标并与已知覆盖校验；没有坐标时要求已知覆盖只有一集；
///   覆盖未知（`WHOLE`/空）时才回落到文件名坐标。任何冲突都返回 `None`，即"这份 sidecar
///   与台账对不上"，不允许它悄悄改写成 Work。
pub fn resolve_nfo_scope(
    layer: NfoLayer,
    video_path: &Path,
    units: &[UnitKey],
    header: &NfoHeader,
) -> Option<EntityScope> {
    let known = known_units(units);
    match layer {
        NfoLayer::Work => is_work_root(&header.root).then_some(EntityScope::Work),
        NfoLayer::Season => {
            if !is_season_root(&header.root) {
                return None;
            }
            let season = directory_season(video_path).or(header.season)?;
            if known
                .iter()
                .any(|(known_season, _)| *known_season != season)
            {
                return None;
            }
            Some(EntityScope::Season(season))
        }
        NfoLayer::Episode => {
            let coords = match (header.season, header.episode) {
                (Some(season), Some(episode)) => {
                    if !known.is_empty() && !known.contains(&(season, episode)) {
                        return None;
                    }
                    (season, episode)
                }
                _ => match known.as_slice() {
                    [(season, episode)] => (*season, *episode),
                    [] => file_coords(video_path)?,
                    _ => return None,
                },
            };
            Some(EntityScope::Episode {
                season: coords.0,
                episode: coords.1,
            })
        }
    }
}

/// 台账覆盖中真正带季集坐标的单元；`UnitKey::WHOLE` 表示"覆盖未知"。
fn known_units(units: &[UnitKey]) -> Vec<(u32, u32)> {
    units
        .iter()
        .filter(|unit| unit.season >= 0 && unit.episode >= 0)
        .map(|unit| (unit.season as u32, unit.episode as u32))
        .collect()
}

fn file_coords(path: &Path) -> Option<(u32, u32)> {
    let name = path.file_name()?.to_str()?;
    let release = release::parse(name);
    Some((release.season?, release.episode?))
}

fn directory_season(path: &Path) -> Option<u32> {
    let name = path.parent()?.file_name()?.to_str()?;
    season_from_directory_name(name)
}

/// `Season 2` / `Season.2` / `S02` / `第2季` → `Some(2)`；`S01E01` 这类文件名不在此列
/// （`number_after_prefix` 只接受紧跟分隔符的数字，`Show 2` 不会误判成第二季）。
fn season_from_directory_name(name: &str) -> Option<u32> {
    let trimmed = name.trim();
    if let Some(index) = trimmed.find('季') {
        let digits: String = trimmed[..index]
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if let Ok(season) = digits.parse() {
            return Some(season);
        }
    }
    let upper = trimmed.to_ascii_uppercase();
    number_after_prefix(&upper, "SEASON").or_else(|| number_after_prefix(&upper, "S"))
}

fn number_after_prefix(text: &str, prefix: &str) -> Option<u32> {
    let rest = text.strip_prefix(prefix)?;
    let rest = rest.trim_start_matches([' ', '.', '_', '-', '+']);
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let tail = &rest[digits.len()..];
    if tail
        .chars()
        .next()
        .is_some_and(|character| character.is_alphanumeric())
    {
        return None;
    }
    digits.parse().ok()
}

fn is_work_root(root: &str) -> bool {
    [
        "movie",
        "tvshow",
        "tvseries",
        "series",
        "video",
        "musicvideo",
    ]
    .iter()
    .any(|candidate| root.eq_ignore_ascii_case(candidate))
}

fn is_season_root(root: &str) -> bool {
    root.eq_ignore_ascii_case("season")
}
