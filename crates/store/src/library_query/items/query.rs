use domain::{LibraryId, UserId};
use rusqlite::types::Value as SqlValue;

use super::filters::filter_clauses;
use crate::{StoreError, library_query::types::BrowseFilter};

/// Bind parameters in order and hand back the `?N` placeholder to write into the SQL.
pub(crate) struct Bindings {
    pub(crate) values: Vec<SqlValue>,
}

impl Bindings {
    pub(crate) fn new() -> Self {
        Self { values: Vec::new() }
    }

    pub(crate) fn from_values(values: Vec<SqlValue>) -> Self {
        Self { values }
    }

    pub(crate) fn push(&mut self, value: SqlValue) -> String {
        self.values.push(value);
        format!("?{}", self.values.len())
    }

    pub(crate) fn placeholders(
        &mut self,
        count: usize,
        value: impl Fn(usize) -> SqlValue,
    ) -> String {
        (0..count)
            .map(|index| self.push(value(index)))
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = SqlValue> + '_ {
        self.values.iter().cloned()
    }
}

/// The FROM/WHERE of one library browse query, shared by the page query and its count.
pub(crate) struct ItemQuerySql {
    pub(crate) from: String,
    pub(crate) where_sql: String,
    pub(crate) bindings: Vec<SqlValue>,
}

impl ItemQuerySql {
    pub(crate) fn build(
        library_id: LibraryId,
        filter: &BrowseFilter,
        user_id: Option<UserId>,
    ) -> Result<Self, StoreError> {
        filter.validate()?;
        let mut bindings = Bindings::new();
        let library = bindings.push(library_id.to_string().into());
        let user = bindings.push(user_id.map(|id| id.to_string()).unwrap_or_default().into());
        let from = format!(
            "library_items i LEFT JOIN user_library_items u \
             ON u.library_id = i.library_id AND u.media_id = i.media_id AND u.user_id = {user}"
        );
        let mut clauses = vec![format!("i.library_id = {library}")];
        clauses.extend(filter_clauses(filter, &mut bindings));
        Ok(Self {
            from,
            where_sql: clauses.join(" AND "),
            bindings: bindings.values,
        })
    }

    pub(crate) fn select(&self, columns: &str) -> String {
        format!(
            "SELECT {columns} FROM {} WHERE {}",
            self.from, self.where_sql
        )
    }
}
