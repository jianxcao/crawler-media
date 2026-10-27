use std::collections::HashSet;

use domain::{LibraryId, ProtocolParent, UserId, UserRole};
use rusqlite::Connection;

use super::LibraryReadError;
use crate::{
    Store, StoreError,
    library_query::{FacetRequest, LibraryQuery, ProtocolItemQuery},
};

#[derive(Clone)]
pub struct ReadCredential {
    user_id: UserId,
    token: String,
}

impl ReadCredential {
    pub fn new(user_id: UserId, token: String) -> Self {
        Self { user_id, token }
    }
}

#[derive(Clone)]
pub enum LibraryReadRequest {
    Browse(LibraryQuery),
    Facets(FacetRequest),
    Protocol(ProtocolItemQuery),
}

impl LibraryReadRequest {
    /// A zero-page Root request carries the complete visible roster through
    /// early folder/header paths without executing a Media query.
    pub fn roster(user_id: UserId) -> Self {
        Self::Protocol(domain::ProtocolItemQuery {
            user_id,
            parent: ProtocolParent::Root,
            series_id: None,
            season: None,
            person_ids: Vec::new(),
            studio_ids: Vec::new(),
            search_term: None,
            genres: Vec::new(),
            genre_ids: Vec::new(),
            filters: Vec::new(),
            is_favorite: None,
            is_played: None,
            include_types: Vec::new(),
            recursive: true,
            is_folder: None,
            sort: Vec::new(),
            offset: 0,
            limit: 0,
        })
    }

    pub(super) fn explicit_library(&self) -> Option<LibraryId> {
        match self {
            Self::Browse(q) => Some(q.library_id),
            Self::Facets(q) => Some(q.library_id),
            Self::Protocol(q) => match q.parent {
                ProtocolParent::Library(id) => Some(id),
                _ => None,
            },
        }
    }
    fn user(&self) -> Option<UserId> {
        match self {
            Self::Browse(q) => q.user_id,
            Self::Facets(q) => q.user_id,
            Self::Protocol(q) => Some(q.user_id),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct RosterEntry {
    id: LibraryId,
    revision: i64,
    lifecycle: String,
    access: String,
    admin_visible: bool,
    members: String,
    locale: String,
    removed: bool,
}

pub struct LibraryReadContext {
    credential: ReadCredential,
    pub(super) request: LibraryReadRequest,
    user: UserId,
    role: UserRole,
    enabled: bool,
    roster: Vec<RosterEntry>,
}

impl LibraryReadContext {
    pub(crate) fn authorized(&self) -> HashSet<LibraryId> {
        self.roster.iter().map(|entry| entry.id).collect()
    }
    pub(crate) fn user(&self) -> UserId {
        self.user
    }
    pub(crate) fn is_admin(&self) -> bool {
        self.role == UserRole::Admin
    }
    /// Whether this prepared authorization roster includes an explicit Library.
    pub fn has_library(&self, id: LibraryId) -> bool {
        self.roster.iter().any(|r| r.id == id)
    }
}

impl Store {
    pub fn prepare_library_read(
        &self,
        credential: &ReadCredential,
        request: &LibraryReadRequest,
    ) -> Result<LibraryReadContext, LibraryReadError> {
        let result = (|| {
            let user = self
                .user_by_token(&credential.token)?
                .ok_or(LibraryReadError::Unauthorized)?;
            if user.id != credential.user_id || request.user().is_some_and(|id| id != user.id) {
                return Err(LibraryReadError::Unauthorized);
            }
            let roster = read_roster(&self.library, user.id, user.role)?;
            Ok(LibraryReadContext {
                credential: credential.clone(),
                request: request.clone(),
                user: user.id,
                role: user.role,
                enabled: user.enabled,
                roster,
            })
        })();
        if let Err(error) = &result {
            log_rejection(
                "prepare",
                credential.user_id,
                request.explicit_library(),
                error,
            );
        }
        result
    }

    pub fn validate_library_read(
        &self,
        context: &LibraryReadContext,
    ) -> Result<(), LibraryReadError> {
        let result = (|| {
            let user = self
                .user_by_token(&context.credential.token)?
                .ok_or(LibraryReadError::Unauthorized)?;
            if user.id != context.user || user.id != context.credential.user_id {
                return Err(LibraryReadError::Unauthorized);
            }
            if user.role != context.role || user.enabled != context.enabled {
                return Err(LibraryReadError::ContextChanged);
            }
            let roster = read_roster(&self.library, user.id, user.role)?;
            if let Some(id) = context.request.explicit_library()
                && context.has_library(id) && !roster.iter().any(|entry| entry.id == id) {
                    return Err(LibraryReadError::Unauthorized);
                }
            if roster != context.roster {
                return Err(LibraryReadError::ContextChanged);
            }
            Ok(())
        })();
        if let Err(error) = &result {
            log_rejection(
                "validate",
                context.user,
                context.request.explicit_library(),
                error,
            );
        }
        result
    }
}

pub(super) fn log_rejection(
    phase: &'static str,
    user: UserId,
    library: Option<LibraryId>,
    error: &LibraryReadError,
) {
    match error {
        LibraryReadError::ContextChanged => {
            tracing::warn!(%user, ?library, phase, error_kind="context_changed", "Library read context changed")
        }
        LibraryReadError::Unauthorized => {
            tracing::error!(%user, ?library, phase, error_kind="unauthorized", "Library read authorization rejected")
        }
        other => {
            tracing::error!(%user, ?library, phase, error_kind="read_error", error=%other, "Library read failed")
        }
    }
}

pub(crate) fn authorized_ids_on(
    conn: &Connection,
    user: UserId,
    role: UserRole,
) -> Result<HashSet<String>, StoreError> {
    Ok(read_roster(conn, user, role)?
        .into_iter()
        .map(|entry| entry.id.to_string())
        .collect())
}

fn read_roster(
    conn: &Connection,
    user: UserId,
    role: UserRole,
) -> Result<Vec<RosterEntry>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT l.library_id,l.config_revision,l.lifecycle_state,l.access_mode,l.admin_visible,l.member_ids_json,l.display_locale,\
         EXISTS(SELECT 1 FROM library_removals r WHERE r.library_id=l.library_id AND r.state!='succeeded')\
         FROM libraries l WHERE l.lifecycle_state IN ('ready','reindexing') ORDER BY l.library_id"
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)? != 0,
            r.get::<_, String>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, i64>(7)? != 0,
        ))
    })?;
    let mut roster = Vec::new();
    for row in rows {
        let (id, revision, lifecycle, access, admin_visible, members, locale, removed) = row?;
        let permitted = if role == UserRole::Admin {
            admin_visible
        } else if access == "everyone" {
            true
        } else {
            serde_json::from_str::<Vec<String>>(&members)
                .unwrap_or_default()
                .contains(&user.to_string())
        };
        if permitted && !removed {
            roster.push(RosterEntry {
                id: id
                    .parse()
                    .map_err(|e| StoreError::Invalid(format!("{e}")))?,
                revision,
                lifecycle,
                access,
                admin_visible,
                members,
                locale,
                removed,
            });
        }
    }
    Ok(roster)
}

pub(super) fn verify_roster(
    conn: &Connection,
    context: &LibraryReadContext,
) -> Result<(), LibraryReadError> {
    let current = read_roster(conn, context.user, context.role)?;
    if let Some(id) = context.request.explicit_library()
        && context.has_library(id) && !current.iter().any(|entry| entry.id == id) {
            return Err(LibraryReadError::Unauthorized);
        }
    if current != context.roster {
        return Err(LibraryReadError::ContextChanged);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
