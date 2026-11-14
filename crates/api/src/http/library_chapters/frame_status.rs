//! Current selected-file availability. GET never repairs durable task state.
use super::*;

#[derive(Deserialize)]
pub(crate) struct FrameStatusQuery {
    file_id: domain::LedgerId,
    season: Option<u32>,
    episode: Option<u32>,
    task_id: Option<String>,
    generation: Option<String>,
}

pub(super) async fn load_current_status(
    state: &ApiState,
    selected: domain::FileCoverage,
) -> Result<(Option<store::ChapterFrameTaskStatus>, bool), Response> {
    let observed =
        super::super::store_wait::store_read(state, &[store::Db::Library], move |store| {
            let current = store
                .file_coverage_by_id(selected.file.file_id)
                .map_err(chapter_store_error)?;
            if !current.is_some_and(|row| {
                row.file.version == selected.file.version
                    && row.file.library_id == selected.file.library_id
                    && row.units == selected.units
            }) {
                return Err(err(
                    StatusCode::CONFLICT,
                    "chapters.source_changed",
                    "所选文件已更新，请重新载入",
                ));
            }
            let targets = store
                .current_chapter_frame_targets(selected.file.file_id)
                .map_err(chapter_store_error)?
                .unwrap_or_default();
            let publication = images::current_publication(store, selected.file.file_id)
                .map_err(chapter_store_error)?;
            Ok((targets.is_empty(), publication))
        })
        .await
        .map_err(|error| error.response())??;
    let (empty, publication) = observed;
    let Some((row, snapshot)) = publication.filter(|_| !empty) else {
        return Ok((None, empty));
    };
    let status = state
        .fs_work
        .reserve(std::time::Duration::from_secs(5))
        .await
        .map_err(|_| super::super::store_wait::busy_response())?
        .run_to_completion(move || project_availability(row, snapshot))
        .await
        .map_err(super::super::library::browse_work_error)?;
    Ok((Some(status), empty))
}

fn project_availability(
    row: domain::FileCoverage,
    snapshot: store::ChapterFrameTaskSnapshot,
) -> store::ChapterFrameTaskStatus {
    let mut status = snapshot.status.clone();
    let mut unavailable = false;
    for target in &mut status.targets {
        if target.ready
            && !images::published_frame(&row, &snapshot, target.index)
                .is_some_and(|(_, path)| crate::chapter_frame_service::worker::valid_frame(&path))
        {
            target.ready = false;
            target.error = Some("published_frame_unavailable".into());
            unavailable = true;
        }
    }
    status.ready = status.targets.iter().filter(|target| target.ready).count();
    status.failed = status
        .targets
        .iter()
        .filter(|target| target.error.is_some())
        .count();
    if unavailable
        && matches!(
            status.state,
            store::ChapterFrameState::Completed
                | store::ChapterFrameState::Partial
                | store::ChapterFrameState::Failed
        )
    {
        status.state = if status.ready == 0 {
            store::ChapterFrameState::Failed
        } else {
            store::ChapterFrameState::Partial
        };
    }
    status
}

pub(crate) async fn frame_status(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<FrameStatusQuery>,
) -> Response {
    if query.task_id.is_some() != query.generation.is_some() {
        return err(
            StatusCode::BAD_REQUEST,
            "chapters.invalid_identity",
            "task_id 与 generation 必须一起提供",
        );
    }
    let media_id = match domain::MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let target = match capture_target(
        &state,
        id,
        media_id,
        user_id,
        ChapterQuery {
            file_id: query.file_id,
            season: query.season,
            episode: query.episode,
        },
    )
    .await
    {
        Ok(target) => target,
        Err(response) => return response,
    };
    match load_current_status(&state, target.row).await {
        Ok((task, empty)) => {
            let superseded = query
                .task_id
                .as_ref()
                .zip(query.generation.as_ref())
                .is_some_and(|(id, generation)| {
                    task.as_ref()
                        .is_none_or(|task| &task.task_id != id || &task.generation != generation)
                });
            ok(json!({"task": task, "empty": empty, "superseded": superseded})).into_response()
        }
        Err(response) => response,
    }
}
