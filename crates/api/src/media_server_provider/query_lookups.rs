//! Query-scoped visible Library lookup and cold cover observation.
use super::*;

pub(super) async fn list_visible_libraries_for_query(
    state: &ApiState,
    principal: media_server::provider::QueryPrincipal,
) -> Result<Vec<ServerLibrary>, media_server::provider::QueryError> {
    let deadline = principal.deadline;
    let (context, libraries) =
        crate::library_read_work::run_store_query_stage(state, deadline, move |store| {
            let user_id = principal.user_id;
            let credential = store::ReadCredential::new(user_id, principal.token);
            let request = store::LibraryReadRequest::roster(user_id);
            let context = store.prepare_library_read(&credential, &request)?;
            let ownership = store
                .library_ownership()
                .map_err(store::LibraryReadError::from)?;
            let role = store
                .user_role(user_id)
                .map_err(store::LibraryReadError::from)?;
            let libraries = store
                .list_libraries()
                .map_err(store::LibraryReadError::from)?;
            Ok((
                context,
                libraries
                    .into_iter()
                    .filter(|lib| {
                        ownership.accepts_work(&lib.id)
                            && if role == "admin" {
                                lib.admin_visible
                            } else {
                                lib.access_mode == "everyone" || lib.member_ids.contains(&user_id)
                            }
                    })
                    .collect::<Vec<_>>(),
            ))
        })
        .await
        .map_err(query_stage_error)?;
    let cold_state = state.clone();
    let header_barrier = cold_state.library_header_barrier.clone();
    let result =
        crate::library_read_work::run_cold_query_stage(&cold_state, deadline, move |budget| {
            let mut result = Vec::with_capacity(libraries.len());
            for lib in libraries {
                budget.check().map_err(query_stage_error)?;
                let cover_path = lib
                    .cover_path
                    .as_deref()
                    .filter(|path| !path.is_empty() && *path != "none")
                    .map(std::path::PathBuf::from)
                    .filter(|path| path.is_file());
                let cover_tag = library_cover_tag(cover_path.as_deref(), lib.kind);
                result.push(ServerLibrary {
                    id: lib.id,
                    name: lib.name,
                    kind: lib.kind,
                    exclude_from_home: lib.exclude_from_home,
                    root_paths: lib.root_paths,
                    cover_path: cover_path.map(|path| path.display().to_string()),
                    cover_tag,
                });
            }
            if let Some(barrier) = &header_barrier {
                barrier();
            }
            Ok(result)
        })
        .await?;
    crate::library_read_work::run_store_query_stage_named(
        state,
        deadline,
        "validate",
        move |store| store.validate_library_read(&context),
    )
    .await
    .map_err(query_stage_error)?;
    state
        .library_reads
        .check_open()
        .map_err(query_stage_error)?;
    media_server::query_metrics::QueryMetrics::page(result.len() as u64, Some(result.len() as u64));
    Ok(result)
}

pub(super) async fn get_user_for_query(
    state: &ApiState,
    principal: media_server::provider::QueryPrincipal,
) -> Result<Option<ServerUser>, media_server::provider::QueryError> {
    let deadline = principal.deadline;
    crate::library_read_work::run_store_query_stage(state, deadline, move |store| {
        let user_id = store
            .user_id_by_token(&principal.token)
            .map_err(store::LibraryReadError::from)?
            .ok_or(store::LibraryReadError::Unauthorized)?;
        if user_id != principal.user_id {
            return Err(store::LibraryReadError::Unauthorized);
        }
        let Some(user) = store
            .get_user(user_id)
            .map_err(store::LibraryReadError::from)?
        else {
            return Ok(None);
        };
        let role = store
            .user_role(user.id)
            .map_err(store::LibraryReadError::from)?;
        Ok(Some(ServerUser {
            id: user.id,
            login: user.login,
            is_admin: role == "admin",
        }))
    })
    .await
    .map_err(query_stage_error)
}
