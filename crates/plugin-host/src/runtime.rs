use crate::{
    HostPorts, Limits, PluginHost,
    workers::{Core, WorkItem},
};
use plugin_api::Failure;
use std::sync::{Arc, mpsc};
use wasmtime::{Config, Engine};

/// One explicit application-owned execution domain. Every host facade borrows its quotas.
#[derive(Clone)]
pub struct PluginRuntime {
    pub(crate) core: Arc<Core>,
    pub(crate) interactive: mpsc::SyncSender<WorkItem>,
    pub(crate) background: mpsc::SyncSender<WorkItem>,
}

impl PluginRuntime {
    pub fn new(limits: Limits) -> Result<Self, Failure> {
        if limits.workers == 0
            || limits.workers > 4
            || limits.queue == 0
            || limits.background_workers == 0
            || limits.background_workers > 4
            || limits.background_queue == 0
            || limits.global_active == 0
            || limits.global_active > 4
            || limits.interactive_reserve > 4
            || !(1..=2).contains(&limits.per_plugin)
            || limits.memory_bytes == 0
            || limits.memory_bytes > 256 * 1024 * 1024
            || limits.total_memory_bytes > 256 * 1024 * 1024
            || limits.memory_bytes > limits.total_memory_bytes
            || limits.wall.is_zero()
            || limits.task_wall.is_zero()
            || limits.endpoint_wall.is_zero()
            || limits.provider_wall.is_zero()
            || limits.fuel == 0
            || limits.background_fuel == 0
            || limits.provider_fuel == 0
            || limits.max_http_calls == 0
        {
            return Err(Failure::Unsupported);
        }
        let mut config = Config::new();
        config
            .wasm_component_model(true)
            .consume_fuel(true)
            .epoch_interruption(true);
        let engine = Arc::new(
            Engine::new(&config).map_err(|error| Failure::InvalidComponent(error.to_string()))?,
        );
        let core = Arc::new(Core::new(engine, limits));
        let channels = Core::start(&core)?;
        Ok(Self {
            core,
            interactive: channels.interactive,
            background: channels.background,
        })
    }

    pub fn host(&self, ports: Arc<dyn HostPorts>) -> PluginHost {
        PluginHost::from_runtime(self, ports)
    }

    pub fn active_plugin_count(&self, plugin_id: &str) -> usize {
        self.core.active_plugin_count(plugin_id)
    }

    pub fn wait_plugin_drained(&self, plugin_id: &str, timeout: std::time::Duration) -> bool {
        self.core.wait_plugin_drained(plugin_id, timeout)
    }
}
