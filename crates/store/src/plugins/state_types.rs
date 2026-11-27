use super::PluginSecurityScope;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginStateKind {
    Kv,
    Blob,
}
impl PluginStateKind {
    pub(super) fn table(self) -> &'static str {
        match self {
            Self::Kv => "plugin_kv",
            Self::Blob => "plugin_blobs",
        }
    }
    pub fn capability(self) -> &'static str {
        match self {
            Self::Kv => "kv",
            Self::Blob => "blob",
        }
    }
    pub(super) fn object_limit(self) -> usize {
        match self {
            Self::Kv => 64 * 1024,
            Self::Blob => 1024 * 1024,
        }
    }
    pub(super) fn total_limit(self) -> i64 {
        match self {
            Self::Kv => 5 * 1024 * 1024,
            Self::Blob => 50 * 1024 * 1024,
        }
    }
    pub(super) fn counter(self) -> &'static str {
        match self {
            Self::Kv => "kv_bytes",
            Self::Blob => "blob_bytes",
        }
    }
}

/// Scope accompanies every owned opaque payload; Debug never prints its bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct PluginScopedBytes {
    pub(super) scope: PluginSecurityScope,
    pub(super) bytes: Vec<u8>,
}
impl std::fmt::Debug for PluginScopedBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginScopedBytes")
            .field("scope", &self.scope)
            .field("length", &self.bytes.len())
            .finish()
    }
}
impl PluginScopedBytes {
    pub fn security_scope(&self) -> &PluginSecurityScope {
        &self.scope
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginStateEntry {
    pub key: String,
    pub revision: i64,
    pub value: PluginScopedBytes,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginStateKey {
    pub key: String,
    pub revision: i64,
    pub bytes: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginQuota {
    pub scope_kv_bytes: i64,
    pub scope_blob_bytes: i64,
    pub plugin_kv_bytes: i64,
    pub plugin_blob_bytes: i64,
}

#[derive(Clone)]
pub enum PluginSecretChange {
    Keep,
    Replace(Vec<u8>),
    Clear,
}
impl std::fmt::Debug for PluginSecretChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keep => f.write_str("Keep"),
            Self::Replace(bytes) => write!(f, "Replace(<{} bytes>)", bytes.len()),
            Self::Clear => f.write_str("Clear"),
        }
    }
}
#[derive(Clone, Debug)]
pub struct PluginSecretUpdate {
    pub key: String,
    pub change: PluginSecretChange,
}
