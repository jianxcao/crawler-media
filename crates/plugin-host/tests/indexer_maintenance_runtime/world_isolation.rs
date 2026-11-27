use crate::support::{
    INDEXER_WASM, MAINTENANCE_WASM, POLICY_WASM, TASK_WASM, encode_import_component,
};
use plugin_api::Failure;
use plugin_host::{Limits, NoServices, PluginRuntime};
use std::sync::Arc;

#[test]
fn component_world_import_isolation_rejects_forbidden_imports() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let host = runtime.host(Arc::new(NoServices));

    // hook-plugin importing browser must fail component validation
    let bad_hook =
        encode_import_component("crawler:plugin/browser@1.0.0", "crawler:plugin/hooks@1.0.0");
    assert!(matches!(
        host.load(&bad_hook, plugin_host::HOOK_WORLD),
        Err(Failure::InvalidComponent(_))
    ));

    // task-plugin importing browser must fail
    let bad_task =
        encode_import_component("crawler:plugin/browser@1.0.0", "crawler:plugin/tasks@1.0.0");
    assert!(matches!(
        host.load_task(&bad_task, plugin_host::TASK_WORLD),
        Err(Failure::InvalidComponent(_))
    ));

    // indexer-plugin importing commands must fail
    let bad_indexer = encode_import_component(
        "crawler:plugin/commands@1.0.0",
        "crawler:plugin/indexer@1.0.0",
    );
    assert!(matches!(
        host.load_indexer(&bad_indexer, plugin_host::INDEXER_WORLD),
        Err(Failure::InvalidComponent(_))
    ));

    // maintenance-plugin importing commands must fail
    let bad_maint = encode_import_component(
        "crawler:plugin/commands@1.0.0",
        "crawler:plugin/maintenance@1.0.0",
    );
    assert!(matches!(
        host.load_maintenance(&bad_maint, plugin_host::MAINTENANCE_WORLD),
        Err(Failure::InvalidComponent(_))
    ));

    // Wrong world rejected
    assert!(matches!(
        host.load_indexer(INDEXER_WASM, plugin_host::MAINTENANCE_WORLD),
        Err(Failure::InvalidComponent(_))
    ));
    assert!(matches!(
        host.load_maintenance(MAINTENANCE_WASM, plugin_host::INDEXER_WORLD),
        Err(Failure::InvalidComponent(_))
    ));
    // Existing policy/task wasm rejected as indexer
    assert!(matches!(
        host.load_indexer(POLICY_WASM, plugin_host::INDEXER_WORLD),
        Err(Failure::InvalidComponent(_))
    ));
    assert!(matches!(
        host.load_indexer(TASK_WASM, plugin_host::INDEXER_WORLD),
        Err(Failure::InvalidComponent(_))
    ));
}
