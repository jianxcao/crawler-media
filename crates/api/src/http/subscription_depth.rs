//! Subscription depth endpoints (CONTEXT.md **Subscribe** depth): wash-cut
//! upgrade reports, removal preview, season cleanup, activity timeline, and
//! today-arrivals. Split out of `subscriptions.rs` (CRUD + JSON) to keep both
//! files under the repo size budget.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::Subscribe;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::http::{err, ok};
use crate::management::ApiState;

mod activity;
mod arrivals;
mod cleanup;
mod forecast;
mod removal_preview;
mod upgrade;
pub(crate) use cleanup::season_cleanup;
pub(crate) use removal_preview::removal_preview;
pub(crate) use upgrade::run_upgrade;
pub(crate) mod wanted;

pub(crate) use activity::activities;
pub(crate) use arrivals::today_arrivals;

use super::subscriptions::{remove_torrents_from_client, user_is_admin};

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunUpgradeInput {
    #[serde(default)]
    pub(crate) filter_id: Option<String>,
    #[serde(default)]
    pub(crate) wash_cut: Option<bool>,
}

/// 洗版目标：(最高优先级原子分数, 人类标签)。来源 = wash_cut_filter_id（缺省
/// 复用 filter_id）；未配置任何原子时目标分数为 0（无法判定）。
pub(crate) fn wash_target(store: &crate::Store, subscribe: &Subscribe) -> (i32, String) {
    let filter_id = subscribe.wash_cut_filter_id.or(Some(subscribe.filter_id));
    let filter = filter_id.and_then(|fid| store.get_filter(fid).ok().flatten());
    // 显式 WashTarget 原子优先（规则面板的「洗到哪一档」）；否则从 resolution/source 原子推导。
    if let Some(filter) = &filter
        && let Some(target) = filter
            .atoms
            .iter()
            .filter_map(|a| match &a.rule {
                domain::AtomRule::WashTarget(v) => Some((v.clone(), a.priority)),
                _ => None,
            })
            .max_by_key(|(_, priority)| *priority)
    {
        return (target.1, target.0);
    }
    let label = filter.as_ref().map(filter_label).unwrap_or_default();
    let score = filter
        .as_ref()
        .map(|f| f.atoms.iter().map(|a| a.priority).max().unwrap_or(0))
        .unwrap_or(0);
    (score, label)
}

/// 一个单元的洗版状态：in_flight / missing / at_cutoff / not_comparable / upgradable。
pub(crate) fn unit_wash_state(
    subscribe: &Subscribe,
    fact: Option<&subscribe::QualityFact>,
    in_flight: bool,
    target_score: i32,
) -> &'static str {
    if !subscribe.wash_cut {
        if fact.is_none() {
            "missing"
        } else {
            "at_cutoff"
        }
    } else if fact.is_none() {
        "missing"
    } else if in_flight {
        "in_flight"
    } else if target_score == 0 {
        "not_comparable"
    } else if fact.map(|f| f.score).unwrap_or(0) >= target_score {
        "at_cutoff"
    } else {
        "upgradable"
    }
}

/// 一个种子标题（Release 解析后）是否覆盖给定单元。
pub(crate) fn release_covers_unit(title: &str, season: Option<u32>, episode: Option<u32>) -> bool {
    let r = release::parse(title);
    match r.season {
        Some(s) if season == Some(s) => match (r.episode, episode) {
            (Some(_from), Some(ep)) => r
                .episode_span()
                .is_some_and(|(from, to)| ep >= from && ep <= to),
            (Some(_), None) => false,
            (None, None) => true,
            (None, Some(_)) => false,
        },
        Some(_) => false,
        None => season.is_none() && episode.is_none(),
    }
}

fn upgrade_summary(
    counts: &std::collections::HashMap<&str, i64>,
    target_label: &str,
    wash_cut: bool,
) -> String {
    let missing = counts["missing"];
    let upgradable = counts["upgradable"];
    let at_cutoff = counts["at_cutoff"];
    if !wash_cut {
        return format!(
            "共 {} 个单元：{} 个已入库，{} 个缺失；未开启洗版，无可升级单元",
            missing + at_cutoff + upgradable,
            at_cutoff,
            missing
        );
    }
    if target_label.is_empty() {
        return format!(
            "共 {} 个单元：{} 个缺失，{} 个已入库；规则组未配置洗版目标，无法判定可升级",
            missing + at_cutoff + upgradable,
            missing,
            at_cutoff
        );
    }
    format!(
        "共 {} 个单元：{} 个缺失，{} 个可升级至 {}，{} 个已达目标",
        missing + at_cutoff + upgradable,
        missing,
        upgradable,
        target_label,
        at_cutoff
    )
}

/// Human label for a Filter: highest-priority resolution + source atoms, e.g. `2160p Remux`.
fn filter_label(filter: &domain::Filter) -> String {
    let mut resolution = None;
    let mut source = None;
    let mut best = i32::MIN;
    for atom in &filter.atoms {
        if atom.priority > best {
            best = atom.priority;
            resolution = None;
            source = None;
        }
        if atom.priority == best {
            match &atom.rule {
                domain::AtomRule::Resolution(v) => resolution = Some(v.as_str()),
                domain::AtomRule::Source(v) => source = Some(v.as_str()),
                _ => {}
            }
        }
    }
    match (resolution, source) {
        (Some(r), Some(s)) => format!("{r} {s}"),
        (Some(r), None) => r.to_string(),
        (None, Some(s)) => s.to_string(),
        (None, None) => String::new(),
    }
}

#[derive(Deserialize)]

pub(crate) struct SeasonCleanupInput {
    #[serde(default)]
    pub(super) seasons: Vec<u32>,
    #[serde(default)]
    pub(super) delete_torrents: bool,
    #[serde(default)]
    pub(super) delete_library_files: bool,
}
