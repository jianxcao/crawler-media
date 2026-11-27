mod patches;
pub mod payload;
mod pipeline;
mod plugins;
mod sanitize;
mod site_policy;
mod trace;
mod trace_gate_notify;

pub use patches::{
    RescoreBudget, rescore_delta_cap, validate_and_apply_download_plan_patch,
    validate_and_apply_search_query_patch, validate_rename_path, validate_select_handles,
};

pub use site_policy::{CheckInOutcome, NexusPhpPolicy, SiteResponsePolicy, reject_challenge};

pub use payload::{
    FilterDiagnostic, NativeCandidate, NativeSearch, SearchOutcome, parse_candidate_published_at_ms,
};
pub use pipeline::{
    Gate, GateContext, GateFailurePolicy, HookPipeline, PluginInvoker, Registration,
    SelectionCandidate, SelectionContext, SiteAction, SiteTransport,
};
pub use plugins::{
    CheckInPlugin, HttpPost, LoginPlugin, PluginError, SiteCredentials, keep_site_alive,
    merge_cookie_string, merge_set_cookies,
};
pub use sanitize::{redact_urls, sanitize_reason};
pub use trace::{HookStepStatus, HookStepTrace, HookTrace};
pub use trace_gate_notify::{NotifyErrorDisposition, NotifyStepOutcome, classify_notify_failure};
