//! Published artifact lookup is a read-only DB phase; all image IO follows it.
use super::*;
use crate::chapter_frame_service::chapter_frame_artifact_path;
use std::path::PathBuf;

#[derive(Deserialize)]
pub(crate) struct ChapterImageQuery {
    generation: String,
    artifact: String,
}

fn artifact_token(artifact: &store::ChapterFrameArtifact) -> String {
    format!("{}-{}", artifact.attempt, artifact.id)
}

pub(super) fn published_frame(
    row: &domain::FileCoverage,
    snapshot: &store::ChapterFrameTaskSnapshot,
    index: usize,
) -> Option<(String, PathBuf)> {
    let target = snapshot
        .targets
        .get(index)
        .filter(|target| target.index == index && target.ready)?;
    snapshot.task.targets.get(index)?;
    let artifact = target.published.as_ref()?;
    let token = artifact_token(artifact);
    Some((
        format!(
            "/chapters/{}/{index}?generation={}&artifact={token}",
            row.file.file_id, snapshot.status.generation
        ),
        chapter_frame_artifact_path(&row.file.path, &snapshot.status.generation, index, artifact),
    ))
}

pub(super) fn current_publication(
    store: &crate::Store,
    file_id: domain::LedgerId,
) -> Result<Option<(domain::FileCoverage, store::ChapterFrameTaskSnapshot)>, store::StoreError> {
    let Some(row) = store.file_coverage_by_id(file_id)? else {
        return Ok(None);
    };
    if row.file.presence != domain::FilePresence::Present
        || row.file.publication != domain::FilePublication::Published
        || !store.library_accepts_work(&row.file.library_id.to_string())?
        || !store
            .get_library(&row.file.library_id.to_string())?
            .is_some_and(|library| library.extract_chapter_images)
        || store.get_media(row.file.media_id)?.is_none()
    {
        return Ok(None);
    }
    let Some(snapshot) = store.current_chapter_frame_task(file_id)? else {
        return Ok(None);
    };
    let targets = store
        .current_chapter_frame_targets(file_id)?
        .unwrap_or_default();
    if snapshot.task.source_version != row.file.version.source_version
        || snapshot.task.targets != targets
        || snapshot.status.generation
            != store::chapter_frame_generation(
                file_id,
                &row.file.version.source_version,
                &targets,
                1,
            )
    {
        return Ok(None);
    }
    Ok(Some((row, snapshot)))
}

pub(super) async fn chapter_items_off_executor(
    state: &ApiState,
    row: domain::FileCoverage,
    chapters: Vec<library::ChapterMarker>,
) -> Result<Vec<Value>, Response> {
    let frames = super::super::store_wait::store_read(state, &[store::Db::Library], move |store| {
        let publication = current_publication(store, row.file.file_id)?.filter(|(current, _)| {
            current.file.version == row.file.version
                && current.file.library_id == row.file.library_id
        });
        Ok::<_, store::StoreError>(
            chapters
                .into_iter()
                .map(|chapter| {
                    let frame = publication.as_ref().and_then(|(current, snapshot)| {
                        snapshot
                            .task
                            .targets
                            .binary_search(&(chapter.start_ms, chapter.end_ms))
                            .ok()
                            .and_then(|index| published_frame(current, snapshot, index))
                    });
                    (chapter, frame)
                })
                .collect::<Vec<_>>(),
        )
    })
    .await
    .map_err(|error| error.response())?
    .map_err(chapter_store_error)?;
    state
        .fs_work
        .reserve(std::time::Duration::from_secs(5))
        .await
        .map_err(|_| super::super::store_wait::busy_response())?
        .run_to_completion(move || frames.into_iter().map(chapter_item).collect())
        .await
        .map_err(super::super::library::browse_work_error)
}

fn chapter_item((chapter, frame): (library::ChapterMarker, Option<(String, PathBuf)>)) -> Value {
    let image_url = frame
        .filter(|(_, path)| crate::chapter_frame_service::worker::valid_frame(path))
        .map(|(url, _)| url);
    let mut value = json!({
        "start_ms": chapter.start_ms, "end_ms": chapter.end_ms,
        "title": chapter.title, "image_url": image_url,
    });
    if let Some(marker_type) = chapter.marker_type {
        value["marker_type"] = json!(match marker_type {
            library::MarkerType::IntroStart => "IntroStart",
            library::MarkerType::IntroEnd => "IntroEnd",
            library::MarkerType::CreditsStart => "CreditsStart",
        });
    }
    value
}

/// Static images remain public, but no URL may resolve another/current identity.
pub(crate) async fn chapter_image(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path((file, index)): Path<(String, String)>,
    Query(query): Query<ChapterImageQuery>,
) -> Response {
    let (Ok(file_id), Ok(index)) = (domain::LedgerId::from_str(&file), index.parse::<usize>())
    else {
        return missing();
    };
    let frame =
        match super::super::store_wait::store_read(&state, &[store::Db::Library], move |store| {
            Ok::<_, store::StoreError>(current_publication(store, file_id)?.and_then(
                |(row, snapshot)| {
                    if snapshot.status.generation != query.generation {
                        return None;
                    }
                    let artifact = snapshot.targets.get(index)?.published.as_ref()?;
                    if artifact_token(artifact) != query.artifact {
                        return None;
                    }
                    published_frame(&row, &snapshot, index).map(|(_, path)| path)
                },
            ))
        })
        .await
        {
            Ok(Ok(Some(frame))) => frame,
            Err(error) => return error.response(),
            Ok(Err(error)) => return chapter_store_error(error),
            _ => return missing(),
        };
    let reservation = match state
        .artwork_reads
        .reserve(super::super::library_artwork::image_response::IMAGE_WAIT)
        .await
    {
        Ok(reservation) => reservation,
        Err(_) => return super::super::store_wait::busy_response(),
    };
    match reservation
        .run(
            super::super::library_artwork::image_response::IMAGE_READ,
            move || {
                std::fs::read(frame).ok().filter(|bytes| {
                    image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).is_ok()
                })
            },
        )
        .await
    {
        Ok(Some(bytes)) => super::super::library_artwork::image_response::conditional_image(
            &headers,
            bytes,
            "image/jpeg",
            true,
        ),
        Err(error) => super::super::library::browse_work_error(error),
        _ => missing(),
    }
}

fn missing() -> Response {
    err(StatusCode::NOT_FOUND, "chapters.missing", "章节帧不存在")
}
