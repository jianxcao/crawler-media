use crate::{JobError, JobKind};
use domain::{MediaId, SubscribeId};
use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobScope {
    Global,
    Subscribe(SubscribeId),
    Media(MediaId),
}

impl fmt::Display for JobScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Global => f.write_str("global"),
            Self::Subscribe(id) => write!(f, "subscribe:{id}"),
            Self::Media(id) => write!(f, "media:{id}"),
        }
    }
}
impl FromStr for JobScope {
    type Err = JobError;
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let scope = if raw == "global" {
            Self::Global
        } else if let Some(id) = raw.strip_prefix("subscribe:") {
            Self::Subscribe(id.parse().map_err(|_| JobError::Identity(raw.into()))?)
        } else if let Some(id) = raw.strip_prefix("media:") {
            Self::Media(id.parse().map_err(|_| JobError::Identity(raw.into()))?)
        } else {
            return Err(JobError::Identity(raw.into()));
        };
        if scope.to_string() != raw {
            return Err(JobError::Identity(raw.into()));
        }
        Ok(scope)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobIdentity {
    pub kind: JobKind,
    pub scope: JobScope,
}
impl JobIdentity {
    pub const fn global(kind: JobKind) -> Self {
        Self {
            kind,
            scope: JobScope::Global,
        }
    }
    pub const fn subscribe_search(id: SubscribeId) -> Self {
        Self {
            kind: JobKind::SubscribeSearch,
            scope: JobScope::Subscribe(id),
        }
    }
    pub const fn catalog_refresh(id: MediaId) -> Self {
        Self {
            kind: JobKind::CatalogRefresh,
            scope: JobScope::Media(id),
        }
    }
    pub(crate) fn validate(self) -> Result<(), JobError> {
        match (self.kind, self.scope) {
            (JobKind::SubscribeSearch, JobScope::Subscribe(_))
            | (JobKind::CatalogRefresh, JobScope::Media(_)) => Ok(()),
            (JobKind::SubscribeSearch | JobKind::CatalogRefresh, _) => {
                Err(JobError::Identity(format!("{:?}", self)))
            }
            (_, JobScope::Global) => Ok(()),
            _ => Err(JobError::Identity(format!("{:?}", self))),
        }
    }
    pub(crate) fn validate_payload(self, payload: &str) -> Result<(), JobError> {
        self.validate()?;
        let (field, expected) = match self.scope {
            JobScope::Global => return Ok(()),
            JobScope::Subscribe(id) => ("subscribe_id", id.to_string()),
            JobScope::Media(id) => ("media_id", id.to_string()),
        };
        let value: serde_json::Value = serde_json::from_str(payload)?;
        if value.get(field).and_then(serde_json::Value::as_str) != Some(expected.as_str()) {
            return Err(JobError::Identity(format!(
                "{field} must match {}",
                self.scope
            )));
        }
        Ok(())
    }
}
