use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Limits {
    pub workers: usize,
    pub queue: usize,
    pub background_workers: usize,
    pub background_queue: usize,
    pub global_active: usize,
    pub interactive_reserve: usize,
    pub per_plugin: usize,
    pub memory_bytes: usize,
    pub total_memory_bytes: usize,
    pub fuel: u64,
    pub background_fuel: u64,
    pub wall: Duration,
    pub task_wall: Duration,
    pub endpoint_wall: Duration,
    pub provider_wall: Duration,
    pub provider_fuel: u64,
    pub max_http_calls: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            workers: 2,
            queue: 32,
            background_workers: 2,
            background_queue: 16,
            global_active: 4,
            interactive_reserve: 1,
            per_plugin: 1,
            memory_bytes: 64 * 1024 * 1024,
            total_memory_bytes: 256 * 1024 * 1024,
            fuel: 10_000_000,
            background_fuel: 50_000_000,
            wall: Duration::from_secs(2),
            task_wall: Duration::from_secs(60),
            endpoint_wall: Duration::from_secs(15),
            provider_wall: Duration::from_secs(15),
            provider_fuel: 50_000_000,
            max_http_calls: 8,
        }
    }
}

impl Limits {
    pub(crate) fn effective_reserve(&self) -> usize {
        if self.global_active > 1 {
            self.interactive_reserve.min(self.global_active - 1)
        } else {
            0
        }
    }
}

pub(crate) struct MemoryBudget {
    pub limit: usize,
    used: usize,
}

impl MemoryBudget {
    pub fn new(limit: usize) -> Self {
        Self { limit, used: 0 }
    }
}

impl wasmtime::ResourceLimiter for MemoryBudget {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let delta = desired.saturating_sub(current);
        if maximum.is_some_and(|max| desired > max)
            || self
                .used
                .checked_add(delta)
                .is_none_or(|sum| sum > self.limit)
        {
            return Err(wasmtime::Error::msg(
                "aggregate linear memory budget exceeded",
            ));
        }
        self.used += delta;
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _: usize,
        desired: usize,
        _: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= 10_000)
    }

    fn instances(&self) -> usize {
        16
    }
    fn tables(&self) -> usize {
        16
    }
    fn memories(&self) -> usize {
        16
    }
}
