use plugin_package::{PackageError, SchemaFieldError};
use store::StoreError;
use store::plugins::PluginError;

#[derive(Debug, thiserror::Error)]
pub enum PluginServiceError {
    #[error("plugin command submission is forbidden in this hook context")]
    WrongContext,
    #[error("unsupported plugin command kind: {0}")]
    UnsupportedCommand(String),
    #[error("{message}")]
    InvalidArgument {
        code: &'static str,
        message: String,
        fields: Vec<SchemaFieldError>,
    },
    #[error("{message}")]
    Unauthenticated { code: &'static str, message: String },
    #[error("{message}")]
    Forbidden { code: &'static str, message: String },
    #[error("{message}")]
    NotFound { code: &'static str, message: String },
    #[error("{message}")]
    PreconditionRequired { code: &'static str, message: String },
    #[error("{message}")]
    Conflict { code: &'static str, message: String },
    #[error("{message}")]
    Unprocessable { code: &'static str, message: String },
    #[error("{message}")]
    PayloadTooLarge { code: &'static str, message: String },
    #[error("{message}")]
    Busy { code: &'static str, message: String },
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Jobs(#[from] jobs::JobError),
    #[error("io failure: {0}")]
    Io(String),
}

impl PluginServiceError {
    pub fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        Self::InvalidArgument {
            code,
            message: message.into(),
            fields: Vec::new(),
        }
    }

    pub fn config_invalid(message: impl Into<String>, fields: Vec<SchemaFieldError>) -> Self {
        Self::InvalidArgument {
            code: "plugin.config_invalid",
            message: message.into(),
            fields,
        }
    }

    pub fn forbidden(code: &'static str, message: impl Into<String>) -> Self {
        Self::Forbidden {
            code,
            message: message.into(),
        }
    }

    pub fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::NotFound {
            code,
            message: message.into(),
        }
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::Conflict {
            code,
            message: message.into(),
        }
    }

    pub fn status_and_code(&self) -> (u16, &'static str) {
        match self {
            Self::WrongContext => (409, "plugin.wrong_context"),
            Self::UnsupportedCommand(_) => (422, "plugin.unsupported_command"),
            Self::InvalidArgument { code, .. } => (400, code),
            Self::Unauthenticated { code, .. } => (401, code),
            Self::Forbidden { code, .. } => (403, code),
            Self::NotFound { code, .. } => (404, code),
            Self::PreconditionRequired { code, .. } => (428, code),
            Self::Conflict { code, .. } => (409, code),
            Self::Unprocessable { code, .. } => (422, code),
            Self::PayloadTooLarge { code, .. } => (413, code),
            Self::Busy { code, .. } => (503, code),
            Self::Store(err) => store_status_and_code(err),
            Self::Jobs(_) => (500, "jobs.error"),
            Self::Io(_) => (500, "plugin.io_error"),
        }
    }

    pub fn fields(&self) -> &[SchemaFieldError] {
        match self {
            Self::InvalidArgument { fields, .. } => fields,
            _ => &[],
        }
    }
}

fn store_status_and_code(err: &StoreError) -> (u16, &'static str) {
    match err {
        StoreError::Plugin(pe) => match pe {
            PluginError::NotFound => (404, "plugin.missing"),
            PluginError::Conflict => (409, "plugin.revision_conflict"),
            PluginError::PackageConflict => (409, "plugin.package_conflict"),
            PluginError::NotDrained => (409, "plugin.not_drained"),
            PluginError::StaleAuthority => (409, "plugin.stale_authority"),
            PluginError::Denied => (403, "plugin.forbidden"),
            PluginError::Invalid(_) => (400, "plugin.invalid_input"),
            PluginError::QuotaExceeded => (409, "plugin.quota_exceeded"),
            PluginError::UnknownEffect => (409, "plugin.unknown_effect"),
            PluginError::EffectDispositionRequired => (409, "plugin.effect_disposition_required"),
            PluginError::CauseLoop => (409, "plugin.cause_loop"),
            PluginError::IdempotencyConflict => (409, "plugin.idempotency_conflict"),
            PluginError::OperationActive(_) => (409, "plugin.operation_active"),
            PluginError::StateSchemaMismatch => (409, "plugin.state_schema_mismatch"),
        },
        StoreError::WriteBusy | StoreError::ReadBusy | StoreError::ReadDeadline => {
            (503, "runtime.busy")
        }
        _ => (500, "store.error"),
    }
}

pub(crate) fn map_package_err(err: PackageError) -> PluginServiceError {
    match err {
        PackageError::LimitExceeded(msg) => PluginServiceError::PayloadTooLarge {
            code: "plugin.package_too_large",
            message: msg,
        },
        PackageError::UiAssetLimitExceeded(msg) => PluginServiceError::Unprocessable {
            code: "plugin.ui_asset_limit_exceeded",
            message: msg,
        },
        PackageError::SignatureRequired => {
            PluginServiceError::forbidden("plugin.signature_required", "package signature required")
        }
        PackageError::UntrustedKey(msg) => {
            PluginServiceError::forbidden("plugin.untrusted_key", msg)
        }
        PackageError::KeyRevoked(msg) => PluginServiceError::forbidden("plugin.key_revoked", msg),
        PackageError::InvalidSignature(msg) => {
            PluginServiceError::forbidden("plugin.invalid_signature", msg)
        }
        PackageError::InvalidConfig { fields } => {
            PluginServiceError::config_invalid("configuration validation failed", fields)
        }
        other => PluginServiceError::invalid("plugin.invalid_package", other.to_string()),
    }
}
