use std::sync::{Arc, Mutex};
use std::time::Duration;

use plugin_api::{
    ApiError, BoostDisk, BoostReservationView, BoostSnapshotView, BoostTaskView, Candidate,
    CommandRequest, FilterInput, FilterPayload, GuestRole, HookContext, Patch,
};
use plugin_host::{HostPorts, Invocation, PluginHost};

pub const POLICY_WASM: &[u8] = include_bytes!("../fixtures/policy.wasm");
pub const TASK_WASM: &[u8] = include_bytes!("../fixtures/task_fixture.wasm");
pub const ENDPOINT_WASM: &[u8] = include_bytes!("../fixtures/endpoint_fixture.wasm");

pub fn ctx(plugin_id: &str) -> HookContext {
    HookContext {
        invocation_id: "inv-host-1".into(),
        cause_id: "cause-host-1".into(),
        plugin_id: plugin_id.into(),
        generation: 1,
        config_revision: 1,
        subject: "subject:1".into(),
        site_id: Some("site-1".into()),
        subscribe_id: Some("sub-1".into()),
        library_id: None,
        deadline_ms: 9_999_999_999,
        cancelled: false,
    }
}

pub fn candidate(handle: &str, title: &str) -> Candidate {
    Candidate {
        handle: handle.into(),
        site_id: "site-1".into(),
        provider: "prov-1".into(),
        generation: 1,
        title: title.into(),
        size: Some(1024),
        seeders: Some(10),
        leechers: Some(1),
        published_at: Some(100),
        free: Some(true),
        hr: Some(false),
        release: Some("1080p".into()),
        score: Some(50),
        labels: vec![],
        revision: 1,
    }
}

pub struct BoostAndCmdPorts {
    pub allow_boost: bool,
    pub captured_commands: Mutex<Vec<CommandRequest>>,
}

impl HostPorts for BoostAndCmdPorts {
    fn authorized(&self, _inv: &Invocation, cap: &str) -> bool {
        if cap == "read:boost" {
            return self.allow_boost;
        }
        true
    }

    fn boost_snapshot(
        &self,
        _inv: &Invocation,
        sites: &[String],
        downloaders: &[String],
    ) -> Result<BoostSnapshotView, ApiError> {
        Ok(BoostSnapshotView {
            revision: 7,
            used_bytes: 2_000,
            reserved_bytes: 500,
            retained_file_bytes: 1_000,
            reservations: vec![BoostReservationView {
                handle: "res-1".into(),
                operation_id: "op-1".into(),
                quota_key: sites.first().cloned().unwrap_or_else(|| "pool:1".into()),
                downloader_id: downloaders
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "dl-1".into()),
                site_id: Some("site-1".into()),
                info_hash: Some("hash-1".into()),
                reserved_bytes: 500,
                stage: "final".into(),
                expires_at: None,
                revision: 1,
            }],
            tasks: vec![BoostTaskView {
                handle: "tf-1".into(),
                quota_key: "pool:1".into(),
                downloader_id: "dl-1".into(),
                site_id: Some("site-1".into()),
                info_hash: "hash-2".into(),
                state: "seeding".into(),
                title: "Example.Torrent".into(),
                progress_permille: 1000,
                downloaded_bytes: 2_000,
                upload_speed: 512,
                download_speed: 0,
                seeding_secs: 600,
                added_at: Some(100),
                completed_at: Some(150),
                last_activity_at: Some(200),
                category: Some("boost".into()),
                tags: vec!["boost".into()],
                task_bytes: 2_000,
                retained_bytes: 0,
                uploaded_bytes: 4_000,
                hr_protected: Some(false),
                hold_until: Some(100),
                free_until: Some(900),
                protection_state: "eligible".into(),
                transfer_active: false,
                library_referenced: false,
                observed_at: 200,
                revision: 2,
            }],
            disk: BoostDisk {
                available_bytes: Some(50_000),
                observed_at: 200,
                status: "ok".into(),
            },
        })
    }

    fn commands_submit(&self, _inv: &Invocation, req: CommandRequest) -> Result<String, ApiError> {
        let mut guard = self.captured_commands.lock().unwrap();
        let idx = guard.len() + 1;
        guard.push(req);
        Ok(format!("op-dl-{idx}"))
    }

    fn boost_run_cycle(&self, _inv: &Invocation, override_json: &str) -> Result<String, ApiError> {
        Ok(format!("{{\"ok\":true,\"override\":{override_json}}}"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenCall {
    pub inv_id: String,
    pub plugin_id: String,
    pub role: GuestRole,
    pub generation: u64,
    pub logged_fields: Vec<(String, String)>,
}

pub struct ContextSpyPorts {
    pub begun: Mutex<Vec<(String, String, GuestRole, u64)>>,
    pub logged: Mutex<Vec<SeenCall>>,
}

impl ContextSpyPorts {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            begun: Mutex::new(Vec::new()),
            logged: Mutex::new(Vec::new()),
        })
    }
}

impl HostPorts for ContextSpyPorts {
    fn begin_invocation(
        &self,
        inv: &Invocation,
    ) -> Result<Option<Box<dyn std::any::Any + Send + Sync>>, ApiError> {
        self.begun.lock().unwrap().push((
            inv.id().to_string(),
            inv.plugin_id.clone(),
            inv.role,
            inv.generation,
        ));
        Ok(None)
    }

    fn authorized(&self, _inv: &Invocation, _cap: &str) -> bool {
        true
    }

    fn log_write(
        &self,
        inv: &Invocation,
        _level: plugin_api::LogLevel,
        message: &str,
        fields: &[(String, String)],
    ) -> Result<(), ApiError> {
        if message == "current-context" {
            self.logged.lock().unwrap().push(SeenCall {
                inv_id: inv.id().to_string(),
                plugin_id: inv.plugin_id.clone(),
                role: inv.role,
                generation: inv.generation,
                logged_fields: fields.to_vec(),
            });
        }
        Ok(())
    }
}

pub fn new_spy_invocation(plugin_id: &str, generation: u64, config_rev: u64) -> Invocation {
    let mut inv = Invocation::new(plugin_id, Duration::from_secs(2));
    inv.generation = generation;
    inv.config_revision = config_rev;
    inv
}

pub fn run_filter_patch(
    host: &PluginHost,
    policy: &plugin_host::CompiledPolicy,
    payload: FilterPayload,
) -> Vec<Patch> {
    host.invoke_filter(
        policy,
        Invocation::new("org.example.policy", Duration::from_secs(2)),
        FilterInput {
            schema_version: 1,
            context: ctx("org.example.policy"),
            payload,
        },
    )
    .unwrap()
}
