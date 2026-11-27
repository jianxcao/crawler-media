use plugin_api::{Event, Failure, FilterInput, FilterOutcome, GateInput, Patch, SearchAfter};

use crate::{CompiledPolicy, Invocation, PluginHost};

pub struct PolicyInvoker {
    host: PluginHost,
    policy: CompiledPolicy,
    invocation: Box<dyn Fn() -> Invocation + Send + Sync>,
}

impl PolicyInvoker {
    pub fn new(
        host: PluginHost,
        policy: CompiledPolicy,
        invocation: impl Fn() -> Invocation + Send + Sync + 'static,
    ) -> Self {
        Self {
            host,
            policy,
            invocation: Box::new(invocation),
        }
    }
}

impl hooks::PluginInvoker for PolicyInvoker {
    fn search_after(&self, input: SearchAfter) -> FilterOutcome {
        self.host.invoke(&self.policy, (self.invocation)(), input)
    }

    fn filter_typed(&self, input: &FilterInput) -> Result<Vec<Patch>, Failure> {
        self.host
            .invoke_filter(&self.policy, (self.invocation)(), input.clone())
    }

    fn gate_typed(&self, input: &GateInput) -> Result<hooks::Gate, String> {
        self.host
            .invoke_gate(&self.policy, (self.invocation)(), input.clone())
            .map_err(|e| e.to_string())
    }

    fn notify_typed(&self, event: &Event) -> Result<(), String> {
        self.notify_typed_result(event).map_err(|e| e.to_string())
    }

    fn notify_typed_result(&self, event: &Event) -> Result<(), Failure> {
        self.host
            .invoke_notify(&self.policy, (self.invocation)(), event.clone())
    }
}
