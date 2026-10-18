use domain::{
    LibraryId, MediaId, PhysicalSourceId, ProtocolItemPage, ProtocolItemQuery, ProtocolItemRef,
    ProtocolItemType, ProtocolParent, ProtocolSortKey, TypedItemId, UnitKey,
};
use rusqlite::params;

use super::super::{Store, StoreError};

impl Store {
    pub fn protocol_item_page(
        &self,
        request: &ProtocolItemQuery,
    ) -> Result<ProtocolItemPage, StoreError> {
        let (lib_scope, unknown_parent) = match &request.parent {
            ProtocolParent::Unknown => (None, true),
            ProtocolParent::Library(lib_id) => (Some(lib_id.to_string()), false),
            _ => (None, false),
        };

        if unknown_parent {
            return Ok(ProtocolItemPage {
                items: Vec::new(),
                total: 0,
            });
        }

        // F02: Determine user's authorized libraries
        let user_id_str = request.user_id.to_string();
        let mut auth_stmt = self.library.prepare(
            "SELECT library_id, access_mode, member_ids_json
             FROM libraries WHERE lifecycle_state = 'ready'",
        )?;
        let mut allowed_libraries: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut auth_rows = auth_stmt.query([])?;
        while let Some(r) = auth_rows.next()? {
            let lid: String = r.get(0)?;
            let mode: String = r.get(1)?;
            let members_json: String = r.get(2)?;
            let is_allowed = if mode == "everyone" {
                true
            } else {
                let members: Vec<String> = serde_json::from_str(&members_json).unwrap_or_default();
                members.contains(&user_id_str)
            };
            if is_allowed {
                allowed_libraries.insert(lid);
            }
        }

        // If explicitly scoped to a library, verify user has access to it
        if let Some(target_lib) = &lib_scope {
            if !allowed_libraries.contains(target_lib) {
                return Ok(ProtocolItemPage {
                    items: Vec::new(),
                    total: 0,
                });
            }
        }

        if allowed_libraries.is_empty() {
            return Ok(ProtocolItemPage {
                items: Vec::new(),
                total: 0,
            });
        }

        // Check if querying for specific series/season/episodes
        let wants_episodes = request.include_types.contains(&ProtocolItemType::Episode)
            || request.series_id.is_some()
            || request.season.is_some();

        if wants_episodes && (request.series_id.is_some() || request.season.is_some()) {
            let (target_media_id, target_season) = if let Some((m, s)) = request.season {
                (m, Some(s))
            } else if let Some(m) = request.series_id {
                (m, None)
            } else {
                unreachable!()
            };

            let media_id_str = target_media_id.to_string();

            // Query units table directly, filtered by authorized library
            let allowed_in_clause = format!(
                "u.library_id IN ({})",
                allowed_libraries.iter().map(|l| format!("'{l}'")).collect::<Vec<_>>().join(",")
            );
            let lib_filter = match &lib_scope {
                Some(l) => format!("u.library_id = '{l}'"),
                None => allowed_in_clause,
            };

            let (count_sql, select_sql) = match target_season {
                Some(s) => (
                    format!(
                        "SELECT COUNT(*) FROM library_units u
                         WHERE u.media_id = ?1 AND u.season = ?2 AND u.season >= 0 AND u.episode >= 0 AND {lib_filter}"
                    ),
                    format!(
                        "SELECT u.library_id, u.media_id, u.season, u.episode, u.preferred_file_id, f.file_revision
                         FROM library_units u
                         LEFT JOIN library_files f ON f.file_id = u.preferred_file_id
                         WHERE u.media_id = ?1 AND u.season = ?2 AND u.season >= 0 AND u.episode >= 0 AND {lib_filter}
                         ORDER BY u.season ASC, u.episode ASC
                         LIMIT ?3 OFFSET ?4"
                    ),
                ),
                None => (
                    format!(
                        "SELECT COUNT(*) FROM library_units u
                         WHERE u.media_id = ?1 AND u.season >= 0 AND u.episode >= 0 AND {lib_filter}"
                    ),
                    format!(
                        "SELECT u.library_id, u.media_id, u.season, u.episode, u.preferred_file_id, f.file_revision
                         FROM library_units u
                         LEFT JOIN library_files f ON f.file_id = u.preferred_file_id
                         WHERE u.media_id = ?1 AND u.season >= 0 AND u.episode >= 0 AND {lib_filter}
                         ORDER BY u.season ASC, u.episode ASC
                         LIMIT ?2 OFFSET ?3"
                    ),
                ),
            };

            let total: i64 = match target_season {
                Some(s) => self.library.query_row(
                    &count_sql,
                    params![media_id_str, s as i64],
                    |r| r.get(0),
                )?,
                None => self.library.query_row(
                    &count_sql,
                    params![media_id_str],
                    |r| r.get(0),
                )?,
            };
            let mut items = Vec::new();
            if total > 0 && request.limit > 0 {
                let mut stmt = self.library.prepare(&select_sql)?;
                let rows_res: Result<Vec<_>, _> = match target_season {
                    Some(s) => stmt.query_map(
                        params![media_id_str, s as i64, request.limit as i64, request.offset as i64],
                        |r| {
                            let lib_id: LibraryId = r.get::<_, String>(0)?.parse().unwrap();
                            let media_id: MediaId = r.get::<_, String>(1)?.parse().unwrap();
                            let season: i32 = r.get(2)?;
                            let episode: i32 = r.get(3)?;
                            let pref_file_str: Option<String> = r.get(4)?;
                            let pref_rev: Option<i64> = r.get(5)?;
                            let preferred_source = pref_file_str.and_then(|id_str| {
                                let file_id = id_str.parse().ok()?;
                                let revision = pref_rev.unwrap_or(1);
                                Some(PhysicalSourceId { file_id, revision })
                            });
                            Ok(ProtocolItemRef {
                                library_id: lib_id,
                                item: TypedItemId::Unit {
                                    media: media_id,
                                    key: UnitKey::new(season, episode),
                                },
                                preferred_source,
                            })
                        },
                    )?.collect(),
                    None => stmt.query_map(
                        params![media_id_str, request.limit as i64, request.offset as i64],
                        |r| {
                            let lib_id: LibraryId = r.get::<_, String>(0)?.parse().unwrap();
                            let media_id: MediaId = r.get::<_, String>(1)?.parse().unwrap();
                            let season: i32 = r.get(2)?;
                            let episode: i32 = r.get(3)?;
                            let pref_file_str: Option<String> = r.get(4)?;
                            let pref_rev: Option<i64> = r.get(5)?;
                            let preferred_source = pref_file_str.and_then(|id_str| {
                                let file_id = id_str.parse().ok()?;
                                let revision = pref_rev.unwrap_or(1);
                                Some(PhysicalSourceId { file_id, revision })
                            });
                            Ok(ProtocolItemRef {
                                library_id: lib_id,
                                item: TypedItemId::Unit {
                                    media: media_id,
                                    key: UnitKey::new(season, episode),
                                },
                                preferred_source,
                            })
                        },
                    )?.collect(),
                };
                items = rows_res?;
            }

            return Ok(ProtocolItemPage {
                items,
                total: total as u64,
            });
        }

        let mut conditions = Vec::new();
        let mut query_params: Vec<rusqlite::types::Value> = Vec::new();
        // Base items query from library_items table scoped to authorized libraries
        if let Some(lib_str) = &lib_scope {
            query_params.push(lib_str.clone().into());
            conditions.push(format!("i.library_id = ?{}", query_params.len()));
        } else {
            let allowed_in = allowed_libraries.iter().map(|l| format!("'{l}'")).collect::<Vec<_>>().join(",");
            conditions.push(format!("i.library_id IN ({allowed_in})"));
        }

        if let Some(search) = &request.search_term {
            let term = format!("%{}%", search.trim().to_lowercase());
            query_params.push(term.into());
            conditions.push(format!("LOWER(i.title) LIKE ?{}", query_params.len()));
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let count_sql = format!("SELECT COUNT(*) FROM library_items i {where_clause}");
        let total: i64 = self.library.query_row(
            &count_sql,
            rusqlite::params_from_iter(query_params.iter().cloned()),
            |r| r.get(0),
        )?;

        let mut order_clause = "ORDER BY i.title_sort ASC, i.media_id ASC";
        if let Some(first_sort) = request.sort.first() {
            match first_sort.key {
                ProtocolSortKey::Name => {
                    order_clause = if first_sort.descending {
                        "ORDER BY i.title_sort DESC, i.media_id ASC"
                    } else {
                        "ORDER BY i.title_sort ASC, i.media_id ASC"
                    };
                }
                ProtocolSortKey::DateCreated => {
                    order_clause = if first_sort.descending {
                        "ORDER BY i.added_at_sort DESC, i.media_id ASC"
                    } else {
                        "ORDER BY i.added_at_sort ASC, i.media_id ASC"
                    };
                }
                _ => {}
            }
        }

        let select_sql = format!(
            "SELECT i.library_id, i.media_id, i.preferred_file_id, f.file_revision
             FROM library_items i
             LEFT JOIN library_files f ON f.file_id = i.preferred_file_id
             {where_clause}
             {order_clause}
             LIMIT ?{} OFFSET ?{}",
            query_params.len() + 1,
            query_params.len() + 2
        );

        query_params.push((request.limit as i64).into());
        query_params.push((request.offset as i64).into());

        let mut items = Vec::new();
        if total > 0 && request.limit > 0 {
            let mut stmt = self.library.prepare(&select_sql)?;
            let rows_res: Result<Vec<_>, _> = stmt.query_map(
                rusqlite::params_from_iter(query_params.iter().cloned()),
                |r| {
                    let lib_id: LibraryId = r.get::<_, String>(0)?.parse().unwrap();
                    let media_id: MediaId = r.get::<_, String>(1)?.parse().unwrap();
                    let pref_file_str: Option<String> = r.get(2)?;
                    let pref_rev: Option<i64> = r.get(3)?;
                    let preferred_source = pref_file_str.and_then(|id_str| {
                        let file_id = id_str.parse().ok()?;
                        let revision = pref_rev.unwrap_or(1);
                        Some(PhysicalSourceId { file_id, revision })
                    });
                    Ok(ProtocolItemRef {
                        library_id: lib_id,
                        item: TypedItemId::Work(media_id),
                        preferred_source,
                    })
                },
            )?.collect();
            items = rows_res?;
        }

        Ok(ProtocolItemPage {
            items,
            total: total as u64,
        })
    }
}
