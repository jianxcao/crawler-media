use cover_generator::{BackgroundOption, CoverOptions, CoverStyle, generate_cover};
use image::DynamicImage;
use std::collections::HashSet;
use std::io::Cursor;
use std::path::Path;

use crate::store::{Library, Store};

/// Old releases copied a single `fanart.jpg` into the generated-cover path.
/// Detect that exact byte-for-byte shape so upgrading preserves manual covers
/// while replacing stale automatic covers when poster art is available.
pub(crate) fn is_legacy_fanart_copy(rows: &[domain::LedgerRow], cover: &Path) -> bool {
    let Ok(cover_bytes) = std::fs::read(cover) else {
        return false;
    };
    rows.iter()
        .filter_map(super::backdrop_path)
        .any(|fanart| std::fs::read(fanart).is_ok_and(|bytes| bytes == cover_bytes))
}

/// Build one landscape cover from poster art for up to six different works.
/// `None` means this Library has no readable local poster yet.
pub(crate) fn generate_library_cover(
    store: &Store,
    library: &Library,
    target: &Path,
) -> Result<Option<std::path::PathBuf>, String> {
    let rows = super::rows_in_library(store, library);
    generate_library_cover_from_rows(library, target, &rows)
}

pub(crate) fn generate_library_cover_from_rows(
    library: &Library,
    target: &Path,
    rows: &[domain::LedgerRow],
) -> Result<Option<std::path::PathBuf>, String> {
    let posters = collect_posters(rows);
    if posters.is_empty() {
        return Ok(None);
    }

    let jpeg = render_library_cover(library, &posters)?;
    write_cover(target, &jpeg)?;
    tracing::info!(
        library_id = %library.id,
        poster_count = posters.len(),
        cover_path = %target.display(),
        "generated library cover from distinct media posters"
    );
    Ok(Some(target.to_path_buf()))
}

fn collect_posters(rows: &[domain::LedgerRow]) -> Vec<DynamicImage> {
    let mut rows: Vec<&domain::LedgerRow> = rows.iter().collect();
    rows.sort_by(|left, right| left.path.cmp(&right.path));
    let mut seen_media_ids = HashSet::new();
    let mut posters: Vec<DynamicImage> = Vec::new();
    for row in rows {
        if seen_media_ids.contains(&row.media_id) {
            continue;
        }
        let Some(path) = super::poster_path(&row) else {
            continue;
        };
        match std::fs::read(&path).and_then(|bytes| {
            image::load_from_memory(&bytes)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        }) {
            Ok(poster) => {
                seen_media_ids.insert(row.media_id);
                posters.push(poster);
                if posters.len() == 6 {
                    break;
                }
            }
            Err(error) => {
                tracing::debug!(%error, poster_path = %path.display(), "skipping unreadable library poster");
            }
        }
    }
    posters
}

fn render_library_cover(library: &Library, posters: &[DynamicImage]) -> Result<Vec<u8>, String> {
    let options = CoverOptions {
        title_zh: library.name.clone(),
        title_en: None,
        width: 1920,
        height: 1080,
        style: CoverStyle::MultiPosterPile,
        background: BackgroundOption::default(),
    };
    let font = include_bytes!("../../../../cover-generator/assets/fonts/chaohei.ttf");
    let generated = generate_cover(&posters, &options, font, None)
        .map_err(|error| format!("cover.generate: {error}"))?;
    let mut jpeg = Vec::new();
    generated
        .write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
        .map_err(|error| format!("cover.encode: {error}"))?;
    Ok(jpeg)
}

fn write_cover(target: &Path, jpeg: &[u8]) -> Result<(), String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("cover target has no parent directory: {}", target.display()))?;
    std::fs::create_dir_all(parent).map_err(|error| {
        format!(
            "failed to create cover directory {}: {error}",
            parent.display()
        )
    })?;
    std::fs::write(target, jpeg).map_err(|error| {
        format!(
            "failed to write library cover {}: {error}",
            target.display()
        )
    })?;
    Ok(())
}
