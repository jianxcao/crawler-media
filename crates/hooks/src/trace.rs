use std::collections::{HashMap, HashSet};
use std::time::Instant;

use plugin_api::{
    Candidate, DownloadPlanView, Failure, FilterAfter, FilterInput, FilterPayload, HookContext,
    Patch, SearchAfter, SearchQuery, SearchSiteAfter, SubscribeScore, SubscribeSelect,
    TransferRename,
};
use serde::{Deserialize, Serialize};

use crate::patches::{
    RescoreBudget, validate_and_apply_download_plan_patch, validate_and_apply_search_query_patch,
    validate_rename_path, validate_select_handles,
};
use crate::pipeline::{DispatchGuard, HookPipeline, Registration};
pub use crate::sanitize::sanitize_reason;

const MAX_TRACE_STEPS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookStepStatus {
    Applied,
    RolledBack,
    Skipped,
    Allowed,
    Rejected,
    Deferred,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookStepTrace {
    pub plugin_id: String,
    pub component: String,
    pub status: HookStepStatus,
    pub decision: String,
    pub reason: Option<String>,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookTrace {
    pub id: String,
    pub hook_id: String,
    pub workflow_id: String,
    pub input_revision: u64,
    #[serde(rename = "plugin_steps")]
    pub steps: Vec<HookStepTrace>,
    pub decision: String,
    pub reason: Option<String>,
    pub duration_ms: u64,
}

pub(super) fn stamp_step_context(
    base: &HookContext,
    reg: &Registration,
    step_idx: usize,
) -> HookContext {
    let mut ctx = base.clone();
    ctx.plugin_id = reg.plugin.clone();
    ctx.generation = reg.generation;
    ctx.config_revision = reg.config_revision;
    ctx.invocation_id = format!("{}:{}:{}", base.invocation_id, reg.plugin, step_idx);
    ctx
}

fn payload_with_candidates(
    template: &FilterPayload,
    candidates: &[Candidate],
    admitted_handles: Option<&[String]>,
) -> FilterPayload {
    match template {
        FilterPayload::SearchSiteAfter(v) => FilterPayload::SearchSiteAfter(SearchSiteAfter {
            site_id: v.site_id.clone(),
            query: v.query.clone(),
            candidates: candidates.to_vec(),
        }),
        FilterPayload::SearchAfter(v) => FilterPayload::SearchAfter(SearchAfter {
            candidates: candidates.to_vec(),
            site_failures: v.site_failures.clone(),
        }),
        FilterPayload::FilterAfter(v) => FilterPayload::FilterAfter(FilterAfter {
            candidates: candidates.to_vec(),
            admitted_handles: admitted_handles
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| v.admitted_handles.clone()),
        }),
        FilterPayload::SubscribeScore(v) => FilterPayload::SubscribeScore(SubscribeScore {
            subscribe_id: v.subscribe_id.clone(),
            media_id: v.media_id.clone(),
            candidates: candidates.to_vec(),
        }),
        FilterPayload::SubscribeSelect(v) => FilterPayload::SubscribeSelect(SubscribeSelect {
            subscribe_id: v.subscribe_id.clone(),
            media_id: v.media_id.clone(),
            candidates: candidates.to_vec(),
            admitted_handles: admitted_handles
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| v.admitted_handles.clone()),
        }),
        other => other.clone(),
    }
}

fn apply_candidate_patches(
    hook_id: &str,
    plugin_id: &str,
    patches: Vec<Patch>,
    candidates: &mut Vec<Candidate>,
    mut admitted: Option<&mut Vec<String>>,
    mut budget: Option<&mut RescoreBudget>,
) -> Result<bool, Failure> {
    let mut set_order_applied = false;
    for patch in patches {
        match patch {
            Patch::Drop(handle) => {
                let Some(pos) = candidates.iter().position(|c| c.handle == handle) else {
                    return Err(Failure::InvalidPatch(
                        "unknown or already dropped handle".into(),
                    ));
                };
                candidates.remove(pos);
                if let Some(adm) = admitted.as_deref_mut() {
                    adm.retain(|h| h != &handle);
                }
            }
            Patch::SetOrder(handles) => {
                if handles.len() != candidates.len() {
                    return Err(Failure::InvalidPatch(
                        "order must contain all surviving handles".into(),
                    ));
                }
                if let Some(adm) = admitted.as_deref() {
                    let allowed_adm: HashSet<&str> = adm.iter().map(String::as_str).collect();
                    if handles.iter().any(|h| !allowed_adm.contains(h.as_str())) {
                        return Err(Failure::InvalidPatch(
                            "order cannot resurrect rejected handle".into(),
                        ));
                    }
                }
                let mut map: HashMap<String, Candidate> = candidates
                    .drain(..)
                    .map(|c| (c.handle.clone(), c))
                    .collect();
                let mut reordered = Vec::with_capacity(handles.len());
                for handle in &handles {
                    let Some(c) = map.remove(handle) else {
                        return Err(Failure::InvalidPatch(
                            "unknown or repeated order handle".into(),
                        ));
                    };
                    reordered.push(c);
                }
                *candidates = reordered;
                if let Some(adm) = admitted.as_deref_mut() {
                    *adm = handles;
                }
                set_order_applied = true;
            }
            Patch::Rescore(rescore) => {
                if let Some(adm) = admitted.as_deref()
                    && !adm.iter().any(|h| h == &rescore.handle)
                {
                    return Err(Failure::InvalidPatch(
                        "cannot rescore rejected handle".into(),
                    ));
                }
                let Some(cand) = candidates.iter_mut().find(|c| c.handle == rescore.handle) else {
                    return Err(Failure::InvalidPatch(
                        "unknown rescore candidate handle".into(),
                    ));
                };
                if let Some(b) = budget.as_deref_mut() {
                    b.try_apply(plugin_id, &rescore.handle, rescore.delta)?;
                }
                let base = cand.score.unwrap_or(0);
                let next = base
                    .checked_add(rescore.delta)
                    .ok_or_else(|| Failure::InvalidPatch("score delta overflow".into()))?;
                cand.score = Some(next);
            }
            Patch::SelectHandles(selected) => {
                if hook_id != "subscribe.select" {
                    return Err(Failure::InvalidPatch(
                        "select-handles only permitted on subscribe.select".into(),
                    ));
                }
                let base_handles: Vec<String> = admitted
                    .as_deref()
                    .map(|a| a.to_vec())
                    .unwrap_or_else(|| candidates.iter().map(|c| c.handle.clone()).collect());
                let valid = validate_select_handles(&base_handles, &selected)?;
                let mut map: HashMap<String, Candidate> = candidates
                    .drain(..)
                    .map(|c| (c.handle.clone(), c))
                    .collect();
                *candidates = valid.iter().filter_map(|h| map.remove(h)).collect();
                if let Some(adm) = admitted.as_deref_mut() {
                    *adm = valid;
                }
            }
            _ => {
                return Err(Failure::InvalidPatch(
                    "unsupported patch variant for candidate hook".into(),
                ));
            }
        }
    }
    Ok(set_order_applied)
}

pub(super) fn push_step(steps: &mut Vec<HookStepTrace>, step: HookStepTrace) {
    if steps.len() < MAX_TRACE_STEPS {
        steps.push(step);
    }
}

#[allow(clippy::too_many_arguments)]
fn run_candidate_filter_step(
    hook_id: &str,
    idx: usize,
    reg: Registration,
    base_context: &HookContext,
    candidates: &mut Vec<Candidate>,
    mut admitted_handles: Option<&mut Vec<String>>,
    mut budget: Option<&mut RescoreBudget>,
    template_payload: &FilterPayload,
) -> (HookStepTrace, bool) {
    let step_started = Instant::now();
    let prev_candidates = candidates.clone();
    let prev_admitted = admitted_handles.as_deref().cloned();
    let prev_budget = budget.as_deref().cloned();
    let input = FilterInput {
        schema_version: 1,
        context: stamp_step_context(base_context, &reg, idx),
        payload: payload_with_candidates(
            template_payload,
            candidates,
            admitted_handles.as_deref().map(Vec::as_slice),
        ),
    };
    let result = reg.invoker.filter_typed(&input).and_then(|patches| {
        let count = patches.len();
        let ordered = apply_candidate_patches(
            hook_id,
            &reg.plugin,
            patches,
            candidates,
            admitted_handles.as_deref_mut(),
            budget.as_deref_mut(),
        )?;
        Ok((count, ordered))
    });
    match result {
        Ok((count, ordered)) => (
            HookStepTrace {
                plugin_id: reg.plugin,
                component: reg.component,
                status: if count == 0 {
                    HookStepStatus::Skipped
                } else {
                    HookStepStatus::Applied
                },
                decision: if count == 0 { "noop" } else { "applied" }.into(),
                reason: None,
                duration_ms: elapsed_ms(step_started),
            },
            ordered,
        ),
        Err(failure) => {
            *candidates = prev_candidates;
            if let (Some(adm), Some(prev)) = (admitted_handles, prev_admitted) {
                *adm = prev;
            }
            if let (Some(b), Some(prev)) = (budget, prev_budget) {
                *b = prev;
            }
            (
                HookStepTrace {
                    plugin_id: reg.plugin,
                    component: reg.component,
                    status: HookStepStatus::RolledBack,
                    decision: "rolled_back".into(),
                    reason: Some(sanitize_reason(&failure.to_string())),
                    duration_ms: elapsed_ms(step_started),
                },
                false,
            )
        }
    }
}

pub(super) fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

impl HookPipeline {
    pub(super) fn matching_registrations(
        &self,
        hook_id: &str,
    ) -> Result<Vec<Registration>, String> {
        let all = self
            .registrations
            .lock()
            .map_err(|_| "Hook registry poisoned".to_string())?
            .clone();
        Ok(all
            .into_iter()
            .filter(|r| {
                r.hook_id
                    .as_deref()
                    .is_none_or(|expected| expected == hook_id)
            })
            .collect())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn filter_candidates_with_trace(
        &self,
        hook_id: &str,
        workflow_id: &str,
        base_context: &HookContext,
        candidates: &mut Vec<Candidate>,
        admitted_handles: Option<&mut Vec<String>>,
        budget: Option<&mut RescoreBudget>,
        template_payload: &FilterPayload,
    ) -> Result<HookTrace, String> {
        self.filter_candidates_with_trace_ordered(
            hook_id,
            workflow_id,
            base_context,
            candidates,
            admitted_handles,
            budget,
            template_payload,
        )
        .map(|(trace, _)| trace)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn filter_candidates_with_trace_ordered(
        &self,
        hook_id: &str,
        workflow_id: &str,
        base_context: &HookContext,
        candidates: &mut Vec<Candidate>,
        mut admitted_handles: Option<&mut Vec<String>>,
        mut budget: Option<&mut RescoreBudget>,
        template_payload: &FilterPayload,
    ) -> Result<(HookTrace, bool), String> {
        let started_all = Instant::now();
        let _guard = DispatchGuard::enter()?;
        let regs = self.matching_registrations(hook_id)?;
        let mut steps = Vec::new();
        let mut set_order_applied = false;

        for (idx, reg) in regs.into_iter().enumerate() {
            let (step, step_ordered) = run_candidate_filter_step(
                hook_id,
                idx,
                reg,
                base_context,
                candidates,
                admitted_handles.as_deref_mut(),
                budget.as_deref_mut(),
                template_payload,
            );
            set_order_applied |= step_ordered;
            push_step(&mut steps, step);
        }

        let trace = HookTrace {
            id: base_context.invocation_id.clone(),
            hook_id: hook_id.to_string(),
            workflow_id: workflow_id.to_string(),
            input_revision: 1,
            steps,
            decision: "applied".to_string(),
            reason: None,
            duration_ms: elapsed_ms(started_all),
        };
        Ok((trace, set_order_applied))
    }

    pub fn filter_search_query_with_trace(
        &self,
        workflow_id: &str,
        base_context: &HookContext,
        query: &mut SearchQuery,
        site_ids: &mut Vec<String>,
    ) -> Result<HookTrace, String> {
        let started_all = Instant::now();
        let _guard = DispatchGuard::enter()?;
        let regs = self.matching_registrations("search.before")?;
        let mut steps = Vec::new();

        for (idx, reg) in regs.into_iter().enumerate() {
            let step_started = Instant::now();
            let prev_query = query.clone();
            let prev_sites = site_ids.clone();
            let input = FilterInput {
                schema_version: 1,
                context: stamp_step_context(base_context, &reg, idx),
                payload: FilterPayload::SearchBefore(plugin_api::SearchBefore {
                    query: query.clone(),
                    site_ids: site_ids.clone(),
                }),
            };
            match reg.invoker.filter_typed(&input) {
                Ok(patches) => {
                    let mut patch_err = None;
                    let mut changed = false;
                    for patch in patches {
                        match patch {
                            Patch::SearchQuery(qp) => {
                                if let Err(err) =
                                    validate_and_apply_search_query_patch(query, site_ids, &qp)
                                {
                                    patch_err = Some(err);
                                    break;
                                }
                                changed = true;
                            }
                            _ => {
                                patch_err = Some(Failure::InvalidPatch(
                                    "unsupported patch variant for search.before".into(),
                                ));
                                break;
                            }
                        }
                    }
                    if let Some(err) = patch_err {
                        *query = prev_query;
                        *site_ids = prev_sites;
                        push_step(
                            &mut steps,
                            HookStepTrace {
                                plugin_id: reg.plugin,
                                component: reg.component,
                                status: HookStepStatus::RolledBack,
                                decision: "rolled_back".into(),
                                reason: Some(sanitize_reason(&err.to_string())),
                                duration_ms: elapsed_ms(step_started),
                            },
                        );
                    } else {
                        push_step(
                            &mut steps,
                            HookStepTrace {
                                plugin_id: reg.plugin,
                                component: reg.component,
                                status: if changed {
                                    HookStepStatus::Applied
                                } else {
                                    HookStepStatus::Skipped
                                },
                                decision: if changed {
                                    "applied".into()
                                } else {
                                    "noop".into()
                                },
                                reason: None,
                                duration_ms: elapsed_ms(step_started),
                            },
                        );
                    }
                }
                Err(err) => {
                    *query = prev_query;
                    *site_ids = prev_sites;
                    push_step(
                        &mut steps,
                        HookStepTrace {
                            plugin_id: reg.plugin,
                            component: reg.component,
                            status: HookStepStatus::RolledBack,
                            decision: "rolled_back".into(),
                            reason: Some(sanitize_reason(&err.to_string())),
                            duration_ms: elapsed_ms(step_started),
                        },
                    );
                }
            }
        }

        Ok(HookTrace {
            id: base_context.invocation_id.clone(),
            hook_id: "search.before".to_string(),
            workflow_id: workflow_id.to_string(),
            input_revision: 1,
            steps,
            decision: "applied".to_string(),
            reason: None,
            duration_ms: elapsed_ms(started_all),
        })
    }

    pub fn filter_download_plan_with_trace(
        &self,
        workflow_id: &str,
        base_context: &HookContext,
        plan: &mut DownloadPlanView,
        allowed_downloaders: &[String],
        template_payload: &FilterPayload,
    ) -> Result<HookTrace, String> {
        let started_all = Instant::now();
        let _guard = DispatchGuard::enter()?;
        let regs = self.matching_registrations("download.before")?;
        let mut steps = Vec::new();

        for (idx, reg) in regs.into_iter().enumerate() {
            let step_started = Instant::now();
            let prev_plan = plan.clone();
            let payload = match template_payload {
                FilterPayload::DownloadBefore(db) => {
                    let mut next = db.clone();
                    next.plan = plan.clone();
                    FilterPayload::DownloadBefore(next)
                }
                other => other.clone(),
            };
            let input = FilterInput {
                schema_version: 1,
                context: stamp_step_context(base_context, &reg, idx),
                payload,
            };
            match reg.invoker.filter_typed(&input) {
                Ok(patches) => {
                    let mut patch_err = None;
                    let mut changed = false;
                    for patch in patches {
                        match patch {
                            Patch::DownloadPlan(dp) => {
                                if let Err(err) = validate_and_apply_download_plan_patch(
                                    plan,
                                    &dp,
                                    allowed_downloaders,
                                    &reg.plugin,
                                ) {
                                    patch_err = Some(err);
                                    break;
                                }
                                changed = true;
                            }
                            _ => {
                                patch_err = Some(Failure::InvalidPatch(
                                    "unsupported patch variant for download.before".into(),
                                ));
                                break;
                            }
                        }
                    }
                    if let Some(err) = patch_err {
                        *plan = prev_plan;
                        push_step(
                            &mut steps,
                            HookStepTrace {
                                plugin_id: reg.plugin,
                                component: reg.component,
                                status: HookStepStatus::RolledBack,
                                decision: "rolled_back".into(),
                                reason: Some(sanitize_reason(&err.to_string())),
                                duration_ms: elapsed_ms(step_started),
                            },
                        );
                    } else {
                        push_step(
                            &mut steps,
                            HookStepTrace {
                                plugin_id: reg.plugin,
                                component: reg.component,
                                status: if changed {
                                    HookStepStatus::Applied
                                } else {
                                    HookStepStatus::Skipped
                                },
                                decision: if changed {
                                    "applied".into()
                                } else {
                                    "noop".into()
                                },
                                reason: None,
                                duration_ms: elapsed_ms(step_started),
                            },
                        );
                    }
                }
                Err(err) => {
                    *plan = prev_plan;
                    push_step(
                        &mut steps,
                        HookStepTrace {
                            plugin_id: reg.plugin,
                            component: reg.component,
                            status: HookStepStatus::RolledBack,
                            decision: "rolled_back".into(),
                            reason: Some(sanitize_reason(&err.to_string())),
                            duration_ms: elapsed_ms(step_started),
                        },
                    );
                }
            }
        }

        Ok(HookTrace {
            id: base_context.invocation_id.clone(),
            hook_id: "download.before".to_string(),
            workflow_id: workflow_id.to_string(),
            input_revision: 1,
            steps,
            decision: "applied".to_string(),
            reason: None,
            duration_ms: elapsed_ms(started_all),
        })
    }

    pub fn filter_transfer_rename_with_trace(
        &self,
        workflow_id: &str,
        base_context: &HookContext,
        input: &mut TransferRename,
    ) -> Result<HookTrace, String> {
        let started_all = Instant::now();
        let _guard = DispatchGuard::enter()?;
        let regs = self.matching_registrations("transfer.rename")?;
        let mut steps = Vec::new();

        for (idx, reg) in regs.into_iter().enumerate() {
            let step = run_transfer_rename_step(idx, reg, base_context, input);
            push_step(&mut steps, step);
        }

        Ok(HookTrace {
            id: base_context.invocation_id.clone(),
            hook_id: "transfer.rename".to_string(),
            workflow_id: workflow_id.to_string(),
            input_revision: 1,
            steps,
            decision: "applied".to_string(),
            reason: None,
            duration_ms: elapsed_ms(started_all),
        })
    }
}

fn run_transfer_rename_step(
    idx: usize,
    reg: Registration,
    base_context: &HookContext,
    input: &mut TransferRename,
) -> HookStepTrace {
    let step_started = Instant::now();
    let prev_path = input.proposed_relative_path.clone();
    let filter_input = FilterInput {
        schema_version: 1,
        context: stamp_step_context(base_context, &reg, idx),
        payload: FilterPayload::TransferRename(input.clone()),
    };
    let result = reg.invoker.filter_typed(&filter_input).and_then(|patches| {
        if patches.is_empty() {
            return Ok(false);
        }
        if patches.len() > 1 {
            return Err(Failure::InvalidPatch(
                "transfer.rename allows at most one patch".into(),
            ));
        }
        match patches.into_iter().next() {
            Some(Patch::RenamePath(next_path)) => {
                input.proposed_relative_path = validate_rename_path(&next_path)?;
                Ok(true)
            }
            _ => Err(Failure::InvalidPatch(
                "unsupported patch variant for transfer.rename".into(),
            )),
        }
    });
    match result {
        Ok(changed) => HookStepTrace {
            plugin_id: reg.plugin,
            component: reg.component,
            status: if changed {
                HookStepStatus::Applied
            } else {
                HookStepStatus::Skipped
            },
            decision: if changed { "applied" } else { "noop" }.into(),
            reason: None,
            duration_ms: elapsed_ms(step_started),
        },
        Err(err) => {
            input.proposed_relative_path = prev_path;
            HookStepTrace {
                plugin_id: reg.plugin,
                component: reg.component,
                status: HookStepStatus::RolledBack,
                decision: "rolled_back".into(),
                reason: Some(sanitize_reason(&err.to_string())),
                duration_ms: elapsed_ms(step_started),
            }
        }
    }
}
