use crate::bindings::crawler::plugin::types;
use plugin_api::{GuestRole, PluginTrust};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

static NEXT_INVOCATION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct Invocation {
    pub plugin_id: String,
    pub provider_id: Option<String>,
    pub generation: u64,
    pub config_revision: u64,
    pub subject: String,
    pub site_id: Option<String>,
    pub credential_epoch: Option<String>,
    pub subscribe_id: Option<String>,
    pub library_id: Option<String>,
    pub cause_id: String,
    pub role: GuestRole,
    pub trust: PluginTrust,
    pub max_http_calls: u32,
    pub deadline: Instant,
    pub(crate) id: String,
    pub(crate) cancellation: Arc<AtomicBool>,
}

impl Invocation {
    pub fn new(plugin_id: impl Into<String>, budget: Duration) -> Self {
        let id = format!(
            "invocation-{}",
            NEXT_INVOCATION.fetch_add(1, Ordering::Relaxed)
        );
        Self {
            plugin_id: plugin_id.into(),
            provider_id: None,
            generation: 1,
            config_revision: 1,
            subject: "system".into(),
            site_id: None,
            credential_epoch: None,
            subscribe_id: None,
            library_id: None,
            cause_id: id.clone(),
            role: GuestRole::Filter,
            trust: PluginTrust::Untrusted,
            max_http_calls: 8,
            deadline: Instant::now() + budget,
            id,
            cancellation: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn cancel(&self) {
        self.cancellation.store(true, Ordering::Release);
    }
    pub fn cancelled(&self) -> bool {
        self.cancellation.load(Ordering::Acquire)
    }
    pub fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }
    pub(crate) fn context(&self, now_ms: u64) -> types::Context {
        types::Context {
            invocation_id: self.id.clone(),
            cause_id: self.cause_id.clone(),
            plugin_id: self.plugin_id.clone(),
            generation: self.generation,
            config_revision: self.config_revision,
            subject: self.subject.clone(),
            site_id: self.site_id.clone(),
            subscribe_id: self.subscribe_id.clone(),
            library_id: self.library_id.clone(),
            cancelled: self.cancelled(),
            deadline_ms: now_ms.saturating_add(
                self.deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as u64,
            ),
        }
    }
}
