use std::cell::Cell;
use std::sync::{Arc, Mutex};

use domain::{LibraryId, MediaId, SiteId, SubscribeId};
use plugin_api::{Event, Failure, FilterInput, FilterOutcome, GateInput, Patch, SearchAfter};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    Allow,
    Abstain,
    Reject(String),
    Defer { until_ms: u64, reason: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SiteAction {
    Authenticate,
    CheckIn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SiteTransport {
    Http,
    Browser,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GateFailurePolicy {
    #[default]
    Open,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionCandidate {
    pub handle: String,
    pub site_id: SiteId,
    pub title: String,
    pub score: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionContext {
    pub subscribe_id: SubscribeId,
    pub media_id: MediaId,
    pub candidates: Vec<SelectionCandidate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateContext {
    Site {
        site_id: SiteId,
        action: SiteAction,
        transport: SiteTransport,
    },
    Download {
        subscribe_id: SubscribeId,
        media_id: MediaId,
        candidate: SelectionCandidate,
    },
    Transfer {
        library_id: LibraryId,
        media_id: MediaId,
        relative_path: std::path::PathBuf,
        score: i32,
    },
    Scrape {
        media_id: MediaId,
        relative_path: std::path::PathBuf,
    },
}

pub trait PluginInvoker: Send + Sync {
    fn gate(&self, _: &GateContext) -> Result<Gate, String> {
        Ok(Gate::Abstain)
    }

    fn search_after(&self, input: SearchAfter) -> FilterOutcome {
        FilterOutcome {
            candidates: input.candidates,
            site_failures: input.site_failures,
            failure: None,
        }
    }

    fn select(&self, context: &SelectionContext) -> Result<Vec<String>, String> {
        Ok(context
            .candidates
            .iter()
            .map(|candidate| candidate.handle.clone())
            .collect())
    }

    fn filter_typed(&self, _input: &FilterInput) -> Result<Vec<Patch>, Failure> {
        Ok(Vec::new())
    }

    fn gate_typed(&self, _input: &GateInput) -> Result<Gate, String> {
        Ok(Gate::Abstain)
    }

    fn notify_typed(&self, _event: &Event) -> Result<(), String> {
        Ok(())
    }

    fn notify_typed_result(&self, event: &Event) -> Result<(), Failure> {
        self.notify_typed(event).map_err(Failure::Trap)
    }
}

impl<F> PluginInvoker for F
where
    F: Fn(&GateContext) -> Result<Gate, String> + Send + Sync,
{
    fn gate(&self, context: &GateContext) -> Result<Gate, String> {
        self(context)
    }
}

#[derive(Clone)]
pub struct Registration {
    pub(crate) priority: i32,
    pub(crate) plugin: String,
    pub(crate) component: String,
    pub(crate) hook_id: Option<String>,
    pub(crate) generation: u64,
    pub(crate) config_revision: u64,
    pub(crate) invoker: Arc<dyn PluginInvoker>,
}

impl Registration {
    pub fn new(
        priority: i32,
        plugin: impl Into<String>,
        component: impl Into<String>,
        invoker: Arc<dyn PluginInvoker>,
    ) -> Self {
        Self {
            priority,
            plugin: plugin.into(),
            component: component.into(),
            hook_id: None,
            generation: 1,
            config_revision: 1,
            invoker,
        }
    }

    pub fn for_hook(
        hook_id: impl Into<String>,
        priority: i32,
        plugin: impl Into<String>,
        component: impl Into<String>,
        invoker: Arc<dyn PluginInvoker>,
    ) -> Self {
        Self {
            priority,
            plugin: plugin.into(),
            component: component.into(),
            hook_id: Some(hook_id.into()),
            generation: 1,
            config_revision: 1,
            invoker,
        }
    }

    pub fn with_metadata(mut self, generation: u64, config_revision: u64) -> Self {
        self.generation = generation;
        self.config_revision = config_revision;
        self
    }
}

thread_local! { static DISPATCHING: Cell<bool> = const { Cell::new(false) }; }

pub(crate) struct DispatchGuard;
impl DispatchGuard {
    pub(crate) fn enter() -> Result<Self, String> {
        if plugin_api::in_guest_execution() {
            return Err("recursive Hook dispatch".into());
        }
        DISPATCHING.with(|active| {
            if active.replace(true) {
                Err("recursive Hook dispatch".into())
            } else {
                Ok(Self)
            }
        })
    }
}
impl Drop for DispatchGuard {
    fn drop(&mut self) {
        DISPATCHING.with(|active| active.set(false));
    }
}

#[derive(Default)]
pub struct HookPipeline {
    pub(crate) registrations: Mutex<Vec<Registration>>,
    pub(crate) failure_policy: GateFailurePolicy,
}

impl HookPipeline {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_failure_policy(failure_policy: GateFailurePolicy) -> Self {
        Self {
            registrations: Mutex::new(Vec::new()),
            failure_policy,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.registrations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }
    pub fn register(&self, registration: Registration) {
        let mut registrations = self.registrations.lock().unwrap_or_else(|e| e.into_inner());
        registrations.push(registration);
        registrations.sort_by(|a, b| {
            (a.priority, &a.plugin, &a.component).cmp(&(b.priority, &b.plugin, &b.component))
        });
    }
    pub fn gate(&self, context: &GateContext) -> Result<Gate, String> {
        let _guard = DispatchGuard::enter()?;
        let registrations = self
            .registrations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for registration in registrations {
            match registration.invoker.gate(context) {
                Ok(Gate::Reject(reason)) => {
                    return Ok(Gate::Reject(crate::sanitize_reason(&reason)));
                }
                Ok(Gate::Defer { until_ms, reason }) => {
                    return Ok(Gate::Defer {
                        until_ms,
                        reason: crate::sanitize_reason(&reason),
                    });
                }
                Ok(Gate::Allow | Gate::Abstain) => {}
                Err(error) => {
                    let clean = crate::sanitize_reason(&error);
                    tracing::warn!(error = %clean, plugin = %registration.plugin, component = %registration.component, "Hook gate failed");
                    if self.failure_policy == GateFailurePolicy::Closed {
                        return Err(clean);
                    }
                }
            }
        }
        Ok(Gate::Allow)
    }

    pub fn gate_typed(&self, input: &GateInput) -> Result<Gate, String> {
        if input.schema_version != 1 {
            return Err("unsupported gate schema_version".into());
        }
        let _guard = DispatchGuard::enter()?;
        let registrations = self
            .registrations
            .lock()
            .map_err(|_| "Hook registry poisoned".to_string())?
            .clone();
        for registration in registrations {
            if let Some(expected) = &registration.hook_id
                && expected != &input.hook_id
            {
                continue;
            }
            match registration.invoker.gate_typed(input) {
                Ok(Gate::Reject(reason)) => {
                    return Ok(Gate::Reject(crate::sanitize_reason(&reason)));
                }
                Ok(Gate::Defer { until_ms, reason }) => {
                    return Ok(Gate::Defer {
                        until_ms,
                        reason: crate::sanitize_reason(&reason),
                    });
                }
                Ok(Gate::Allow | Gate::Abstain) => {}
                Err(error) => {
                    let clean = crate::sanitize_reason(&error);
                    tracing::warn!(
                        error = %clean,
                        plugin = %registration.plugin,
                        component = %registration.component,
                        hook_id = %input.hook_id,
                        "Typed hook gate failed"
                    );
                    if self.failure_policy == GateFailurePolicy::Closed {
                        return Err(clean);
                    }
                }
            }
        }
        Ok(Gate::Allow)
    }

    pub fn filter_typed(
        &self,
        hook_id: &str,
        input: &FilterInput,
    ) -> Result<(Vec<Patch>, Vec<crate::FilterDiagnostic>), String> {
        if input.schema_version != 1 {
            return Err("unsupported filter schema_version".into());
        }
        let _guard = DispatchGuard::enter()?;
        let registrations = self
            .registrations
            .lock()
            .map_err(|_| "Hook registry poisoned".to_string())?
            .clone();
        let mut patches = Vec::new();
        let mut diagnostics = Vec::new();
        for registration in registrations {
            if let Some(expected) = &registration.hook_id
                && expected != hook_id
            {
                continue;
            }
            match registration.invoker.filter_typed(input) {
                Ok(mut out) => patches.append(&mut out),
                Err(failure) => {
                    let clean = crate::sanitize_reason(&failure.to_string());
                    tracing::warn!(
                        failure = %clean,
                        plugin = %registration.plugin,
                        component = %registration.component,
                        hook_id,
                        "Typed hook filter failed"
                    );
                    diagnostics.push(crate::FilterDiagnostic {
                        plugin: registration.plugin,
                        component: registration.component,
                        failure,
                    });
                }
            }
        }
        Ok((patches, diagnostics))
    }

    pub fn notify_typed(&self, event: &Event) -> Vec<(String, String, String)> {
        if event.schema_version != 1 {
            return vec![(
                "host".into(),
                "notify".into(),
                "unsupported event schema_version".into(),
            )];
        }
        let Ok(_guard) = DispatchGuard::enter() else {
            return vec![(
                "host".into(),
                "notify".into(),
                "recursive Hook dispatch".into(),
            )];
        };
        let Ok(registrations) = self.registrations.lock().map(|g| g.clone()) else {
            return vec![(
                "host".into(),
                "notify".into(),
                "Hook registry poisoned".into(),
            )];
        };
        let mut errors = Vec::new();
        for registration in registrations {
            if let Some(expected) = &registration.hook_id
                && expected != &event.kind
            {
                continue;
            }
            if let Err(err) = registration.invoker.notify_typed(event) {
                let clean = crate::sanitize_reason(&err);
                tracing::warn!(
                    error = %clean,
                    plugin = %registration.plugin,
                    component = %registration.component,
                    event_kind = %event.kind,
                    "Typed hook notify failed"
                );
                errors.push((registration.plugin, registration.component, clean));
            }
        }
        errors
    }
    pub fn search_after(&self, mut input: SearchAfter) -> Result<crate::SearchOutcome, String> {
        let _guard = DispatchGuard::enter()?;
        let mut handles = std::collections::HashSet::new();
        if input.candidates.iter().any(|candidate| {
            candidate.handle.is_empty() || !handles.insert(candidate.handle.clone())
        }) {
            return Err("invalid candidate handles".into());
        }
        let registrations = self
            .registrations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut diagnostics = Vec::new();
        for registration in registrations {
            let output = registration.invoker.search_after(input.clone());
            if let Some(failure) = output.failure {
                let clean = crate::sanitize_reason(&failure.to_string());
                tracing::warn!(failure = %clean, plugin = %registration.plugin, component = %registration.component, "Hook filter failed");
                diagnostics.push(crate::FilterDiagnostic {
                    plugin: registration.plugin,
                    component: registration.component,
                    failure,
                });
                continue;
            }
            let allowed: std::collections::HashMap<_, _> =
                input.candidates.iter().map(|c| (&c.handle, c)).collect();
            let mut seen = std::collections::HashSet::new();
            if output.site_failures != input.site_failures
                || output
                    .candidates
                    .iter()
                    .any(|c| allowed.get(&c.handle) != Some(&c) || !seen.insert(&c.handle))
            {
                tracing::warn!("Hook returned invalid candidate patch");
                diagnostics.push(crate::FilterDiagnostic {
                    plugin: registration.plugin,
                    component: registration.component,
                    failure: plugin_api::Failure::InvalidPatch("invalid candidate output".into()),
                });
                continue;
            }
            input.candidates = output.candidates;
        }
        Ok(crate::SearchOutcome {
            candidates: input.candidates,
            site_failures: input.site_failures,
            diagnostics,
        })
    }
    pub fn select(&self, mut context: SelectionContext) -> Result<Vec<String>, String> {
        let _guard = DispatchGuard::enter()?;
        let registrations = self
            .registrations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for registration in registrations {
            match registration.invoker.select(&context) {
                Ok(next) => {
                    let allowed: std::collections::HashSet<_> =
                        context.candidates.iter().map(|c| &c.handle).collect();
                    let mut seen = std::collections::HashSet::new();
                    if next
                        .iter()
                        .all(|handle| allowed.contains(handle) && seen.insert(handle))
                    {
                        let mut originals: std::collections::HashMap<_, _> = context
                            .candidates
                            .into_iter()
                            .map(|c| (c.handle.clone(), c))
                            .collect();
                        context.candidates = next
                            .into_iter()
                            .filter_map(|handle| originals.remove(&handle))
                            .collect();
                    } else {
                        tracing::warn!("Hook returned invalid selection");
                    }
                }
                Err(error) => {
                    let clean = crate::sanitize_reason(&error);
                    tracing::warn!(error = %clean, "Hook selection failed");
                }
            }
        }
        Ok(context
            .candidates
            .into_iter()
            .map(|candidate| candidate.handle)
            .collect())
    }
}
