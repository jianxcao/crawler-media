pub const API_VERSION: &str = "1.0.0";
pub const HOOK_WORLD: &str = "crawler:plugin/hook-plugin@1.0.0";
pub const TASK_WORLD: &str = "crawler:plugin/task-plugin@1.0.0";
pub const ENDPOINT_WORLD: &str = "crawler:plugin/endpoint-plugin@1.0.0";
pub const INDEXER_WORLD: &str = "crawler:plugin/indexer-plugin@1.0.0";
pub const MAINTENANCE_WORLD: &str = "crawler:plugin/maintenance-plugin@1.0.0";
/// Executable runtime worlds; WIT's future service definitions do not imply availability.
pub const SUPPORTED_WORLDS: &[&str] = &[
    HOOK_WORLD,
    TASK_WORLD,
    ENDPOINT_WORLD,
    INDEXER_WORLD,
    MAINTENANCE_WORLD,
];
pub const SUPPORTED_HOOKS: &[&str] = &[
    "search.before",
    "search.site.after",
    "search.after",
    "filter.after",
    "subscribe.score",
    "subscribe.select",
    "download.before",
    "download.after",
    "transfer.rename",
    "transfer.completed",
    "site.checkin.after",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookKind {
    Filter,
    Gate,
    Notify,
    FilterOrGate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct HookCatalogEntry {
    pub id: &'static str,
    pub kind: HookKind,
    pub phase: u8,
    pub description: &'static str,
}

pub const HOOK_CATALOG: &[HookCatalogEntry] = &[
    HookCatalogEntry {
        id: "search.before",
        kind: HookKind::FilterOrGate,
        phase: 1,
        description: "Inspect, rewrite, or gate search query and site list before dispatch",
    },
    HookCatalogEntry {
        id: "search.site.after",
        kind: HookKind::Filter,
        phase: 1,
        description: "Filter or reorder raw candidates from a single site search",
    },
    HookCatalogEntry {
        id: "search.after",
        kind: HookKind::Filter,
        phase: 1,
        description: "Filter or reorder merged search candidates across sites",
    },
    HookCatalogEntry {
        id: "filter.after",
        kind: HookKind::Filter,
        phase: 1,
        description: "Adjust scores or drop candidates after host filter admission",
    },
    HookCatalogEntry {
        id: "subscribe.score",
        kind: HookKind::Filter,
        phase: 1,
        description: "Apply bounded score deltas to Subscribe candidates",
    },
    HookCatalogEntry {
        id: "subscribe.select",
        kind: HookKind::FilterOrGate,
        phase: 1,
        description: "Select or gate final admitted handles for a Subscribe round",
    },
    HookCatalogEntry {
        id: "download.before",
        kind: HookKind::FilterOrGate,
        phase: 1,
        description: "Patch download plan or allow/reject/defer before downloader submission",
    },
    HookCatalogEntry {
        id: "download.after",
        kind: HookKind::Notify,
        phase: 1,
        description: "Notify plugins after download submission outcome settles",
    },
    HookCatalogEntry {
        id: "transfer.rename",
        kind: HookKind::Filter,
        phase: 1,
        description: "Propose normalized relative target path before LibraryWorkLease acquisition",
    },
    HookCatalogEntry {
        id: "transfer.completed",
        kind: HookKind::Notify,
        phase: 1,
        description: "Notify plugins after committed library transfer publication",
    },
    HookCatalogEntry {
        id: "site.checkin.after",
        kind: HookKind::Notify,
        phase: 1,
        description: "Notify plugins after site check-in maintenance completion",
    },
];
