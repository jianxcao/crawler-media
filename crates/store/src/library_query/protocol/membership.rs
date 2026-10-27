//! Exact selected-locale memberships, with person aliases resolved inside Scope.
//!
//! The person filter is the one membership that needs a *derived* identity set: a
//! request names people by `wire_id`/`tmdb_id`, and a card matches when one of its
//! own person rows carries the same role and either the same display name or (for
//! Actors) the same TMDB id. That set has to be resolved from the Scoped cards
//! before any card can be tested, so it is materialised once and the per-card
//! predicate probes it — never a re-scan of the Library's people table.
//! See `docs/audits/2026-10-26-library-schema-audit.md` (H2, R6).
use super::{request::NormalizedProtocolRequest, scope::Scope};
use crate::{StoreError, library_query::items::Bindings};
use rusqlite::types::Value;

/// A membership predicate plus the CTEs it needs.
pub(super) struct Membership {
    /// `None` when the predicate is self-contained. Otherwise a `name AS …` fragment
    /// that must be placed **after** `scoped` (it reads that CTE) and **before**
    /// `rows` (that CTE reads this one).
    pub(super) ctes: Option<String>,
    pub(super) predicate: String,
}

pub(super) fn exact_predicates(
    request: &NormalizedProtocolRequest,
    _scope: &Scope,
    bindings: &mut Bindings,
) -> Result<Membership, StoreError> {
    let mut clauses = Vec::new();
    for (kind, keys) in [("genre", &request.genres), ("studio", &request.studios)] {
        if keys.is_empty() {
            continue;
        }
        let list = list(bindings, keys);
        clauses.push(format!("EXISTS (SELECT 1 FROM library_protocol_terms t WHERE t.library_id=s.library_id AND t.media_id=s.media_id AND t.term_kind='{kind}' AND t.term_value IN ({list}))"));
    }
    let ctes = if request.people.is_empty() {
        None
    } else {
        let (extra, clause) = person_filter(bindings, &request.people);
        clauses.push(clause);
        Some(extra)
    };
    let predicate = if clauses.is_empty() {
        "1 = 1".to_string()
    } else {
        clauses.join(" AND ")
    };
    Ok(Membership { ctes, predicate })
}

/// Resolve the requested identities once, then probe per card.
///
/// Before this, the per-card subquery carried the `wire_id IN (…) OR tmdb_id IN (…)`
/// test itself, and that `OR` blocked both person indexes: the planner answered
/// `SEARCH a USING PRIMARY KEY (library_id=?)`, i.e. one walk of the Library's whole
/// people table **per card** (measured 0.91 s for 1 card, 9.1 s for 10, ≈30 min at
/// 20k cards on a 200k-person Library).
///
/// Three things keep the result set identical:
/// 1. `person_wanted` is still restricted to the Scoped cards (`JOIN scoped seed`), so
///    an identity only counts when it appears on a card that passed the other filters;
/// 2. the name is compared as `p.name_key = trim(a.name_key)`, exactly as before;
/// 3. the TMDB identity is only carried for Actors with a non-empty `tmdb_id`, which is
///    what the old `a.role='Actor' AND a.tmdb_id<>''` branch tested.
///
/// `DISTINCT` is part of the fix, not tidiness: an identity that appears on many cards
/// has one people row *per card*, so without it the materialised set would grow with the
/// Library and every per-card probe would walk it again. Collapsed to distinct
/// identities, its size is bounded by the request, not by the Library.
///
/// Cost: one materialised resolution (two index probes), then one probe over a handful
/// of identities per person row of each card — independent of the Library's size.
///
/// The Scope check is a correlated `EXISTS` rather than a `JOIN scoped seed`: with a
/// join the planner makes `a` the *inner* table and walks the Library's people by
/// primary-key prefix, which is the very scan this task removes. With `EXISTS` the
/// `wire_id/tmdb_id` disjunction stays outermost and SQLite answers it with a
/// `MULTI-INDEX OR` over `protocol_people_wire` **and** `protocol_people_tmdb`
/// (pinned in `tests/library_query_plans.rs`).
fn person_filter(bindings: &mut Bindings, people: &[String]) -> (String, String) {
    let wanted = list(bindings, people);
    let ctes = format!(
        "person_wanted AS MATERIALIZED (\
           SELECT DISTINCT a.role AS role, trim(a.name_key) AS name_key, \
                  CASE WHEN a.role='Actor' AND a.tmdb_id<>'' THEN a.tmdb_id ELSE '' END AS tmdb_id \
             FROM library_protocol_people a \
            WHERE (a.wire_id IN ({wanted}) OR a.tmdb_id IN ({wanted})) \
              AND EXISTS (SELECT 1 FROM scoped seed \
                           WHERE seed.library_id=a.library_id AND seed.media_id=a.media_id))"
    );
    let clause = "EXISTS (\
           SELECT 1 FROM library_protocol_people p \
            WHERE p.library_id=s.library_id AND p.media_id=s.media_id \
              AND EXISTS (SELECT 1 FROM person_wanted w \
                           WHERE w.role=p.role \
                             AND (w.name_key=p.name_key \
                                  OR (w.tmdb_id<>'' AND w.tmdb_id=p.tmdb_id))))"
        .to_string();
    (ctes, clause)
}

fn list(bindings: &mut Bindings, keys: &[String]) -> String {
    keys.iter()
        .map(|v| bindings.push(Value::Text(v.clone())))
        .collect::<Vec<_>>()
        .join(", ")
}
