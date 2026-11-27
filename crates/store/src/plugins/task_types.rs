#[derive(Clone, Debug, PartialEq)]
pub struct PluginTaskDefinition {
    pub enabled: bool,
    pub interval_secs: Option<i64>,
    /// Validated public config/slot references only, never secret bytes.
    pub public_config: serde_json::Value,
}
#[derive(Clone, Debug, PartialEq)]
pub struct PluginTaskBinding {
    pub binding_id: uuid::Uuid,
    pub plugin_id: String,
    pub task_id: String,
    pub definition_revision: i64,
    pub generation: i64,
    pub definition: PluginTaskDefinition,
    pub projection_revision: i64,
    pub projection_generation: Option<i64>,
    pub job_def_id: Option<String>,
}
impl PluginTaskBinding {
    pub fn projection_pending(&self) -> bool {
        self.projection_revision != self.definition_revision
            || self.projection_generation != Some(self.generation)
    }
}
