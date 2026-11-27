#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveLimits {
    pub max_package_bytes: u64,
    pub max_entries: usize,
    pub max_entry_uncompressed_bytes: u64,
    pub max_total_uncompressed_bytes: u64,
    pub max_compression_ratio: u64,
    pub ratio_check_min_bytes: u64,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_package_bytes: 64 * 1024 * 1024,
            max_entries: 2048,
            max_entry_uncompressed_bytes: 32 * 1024 * 1024,
            max_total_uncompressed_bytes: 128 * 1024 * 1024,
            max_compression_ratio: 100,
            ratio_check_min_bytes: 4096,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedKey {
    pub key_id: String,
    pub public_key: String,
    pub revoked: bool,
}

#[derive(Clone, Debug)]
pub struct PackagePolicy {
    pub api_version: String,
    pub allowed_worlds: Vec<String>,
    pub allowed_hooks: Vec<String>,
    pub allowed_events: Vec<String>,
    pub allowed_commands: Vec<String>,
    pub allowed_endpoint_auths: Vec<String>,
    pub ui_contract: String,
    pub react_version: String,
    pub react_dom_version: String,
    pub allowed_ui_icons: Vec<String>,
    pub reserved_remote_names: Vec<String>,
    pub dev_mode: bool,
    pub detached_signature: Option<String>,
    pub trust_keys: Vec<TrustedKey>,
    pub limits: ArchiveLimits,
}

pub const DEFAULT_ALLOWED_COMMANDS: &[&str] = &[
    "indexer.search",
    "indexer.rss",
    "site.maintenance",
    "site.check-in",
    "site.verify-session",
    "downloader.add",
    "downloader.control",
    "downloader.delete-task",
    "downloader.prepare-cleanup",
    "downloader.delete-files",
    "boost.run",
    "notify.send",
    "schedule.set",
    "schedule.disable",
];

pub const DEFAULT_ALLOWED_ENDPOINT_AUTHS: &[&str] = &["admin", "user"];

pub const DEFAULT_ALLOWED_UI_ICONS: &[&str] = &[
    "calendar-check",
    "activity",
    "shield",
    "sliders",
    "download",
    "search",
    "puzzle",
    "database",
    "bell",
    "file-text",
    "gauge",
    "check-circle",
];

pub const DEFAULT_ALLOWED_EVENTS: &[&str] = &[
    "site.checkin.after",
    "download.after",
    "transfer.completed",
    "download.completed",
    "subscribe.completed",
    "library.item-added",
];

pub const DEFAULT_RESERVED_REMOTE_NAMES: &[&str] = &[
    // ECMAScript reserved keywords & future reserved words
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "let",
    "static",
    "yield",
    "await",
    "implements",
    "interface",
    "package",
    "private",
    "protected",
    "public",
    // Strict-mode restricted identifiers & non-configurable ES globals
    "eval",
    "arguments",
    "undefined",
    "NaN",
    "Infinity",
    // Object.prototype & host/browser globals
    "constructor",
    "__proto__",
    "prototype",
    "toString",
    "valueOf",
    "hasOwnProperty",
    "window",
    "globalThis",
    "self",
    "document",
    "top",
    "parent",
    "frames",
    "location",
    "navigator",
    "host",
    "crawler_media",
    "web",
    // Built-in JS / module / framework globals
    "Object",
    "Array",
    "Function",
    "Promise",
    "Symbol",
    "Proxy",
    "Reflect",
    "Map",
    "Set",
    "Error",
    "JSON",
    "Math",
    "Date",
    "RegExp",
    "fetch",
    "console",
    "require",
    "module",
    "exports",
    "global",
    "process",
    "Buffer",
    "React",
    "ReactDOM",
];

impl Default for PackagePolicy {
    fn default() -> Self {
        Self {
            api_version: plugin_api::API_VERSION.to_string(),
            allowed_worlds: plugin_api::SUPPORTED_WORLDS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            allowed_hooks: plugin_api::SUPPORTED_HOOKS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            allowed_events: DEFAULT_ALLOWED_EVENTS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            allowed_commands: DEFAULT_ALLOWED_COMMANDS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            allowed_endpoint_auths: DEFAULT_ALLOWED_ENDPOINT_AUTHS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            ui_contract: "1.0.0".to_string(),
            react_version: "19.3.0".to_string(),
            react_dom_version: "19.3.0".to_string(),
            allowed_ui_icons: DEFAULT_ALLOWED_UI_ICONS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            reserved_remote_names: DEFAULT_RESERVED_REMOTE_NAMES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            dev_mode: false,
            detached_signature: None,
            trust_keys: Vec::new(),
            limits: ArchiveLimits::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignatureStatus {
    Trusted { key_id: String, fingerprint: String },
    UnsignedDevMode,
}

impl SignatureStatus {
    pub fn as_store_str(&self) -> &'static str {
        match self {
            Self::Trusted { .. } => "trusted",
            Self::UnsignedDevMode => "development",
        }
    }

    pub fn signing_key_id(&self) -> Option<&str> {
        match self {
            Self::Trusted { key_id, .. } => Some(key_id.as_str()),
            Self::UnsignedDevMode => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureVerification {
    pub status: SignatureStatus,
    pub package_sha256_hex: String,
}
