use cover_generator::{BackgroundOption, CoverOptions, CoverStyle, generate_cover};
use image::DynamicImage;
use std::collections::HashSet;
use std::io::Cursor;

use crate::store::Library;

/// 自动封面默认选项（标题取库名、多海报拼贴）。
pub(crate) fn default_cover_options(library: &Library) -> CoverOptions {
    CoverOptions {
        title_zh: library.name.clone(),
        title_en: None,
        width: 1920,
        height: 1080,
        style: CoverStyle::MultiPosterPile,
        background: BackgroundOption::default(),
    }
}

/// 锁外渲染：读取本地海报并合成 JPEG 字节，**不写盘**（发布由 cover service 负责）。
/// `None` means this Library has no readable local poster yet.
pub(crate) fn render_cover_jpeg(
    library: &Library,
    files: &[domain::LibraryFile],
    options: &CoverOptions,
) -> Result<Option<Vec<u8>>, String> {
    let posters = collect_posters(files);
    if posters.is_empty() {
        return Ok(None);
    }
    let jpeg = render_library_cover(&posters, options)?;
    tracing::info!(
        library_id = %library.id,
        poster_count = posters.len(),
        "rendered library cover bytes from distinct media posters"
    );
    Ok(Some(jpeg))
}

/// 只用到文件级事实（path / poster_path / media_id），所以收 `LibraryFile`；旧读模型里
/// 同一文件的多个 unit 是重复行，`seen_media_ids` 去重后结果一致。排序仍按旧
/// `row.path: String` 的逐字节序（`PathBuf` 是分量序，不能直接用）。
fn collect_posters(files: &[domain::LibraryFile]) -> Vec<DynamicImage> {
    let mut files: Vec<&domain::LibraryFile> = files.iter().collect();
    files.sort_by(|left, right| {
        left.path
            .to_string_lossy()
            .cmp(&right.path.to_string_lossy())
    });
    let mut seen_media_ids = HashSet::new();
    let mut posters: Vec<DynamicImage> = Vec::new();
    for file in files {
        if seen_media_ids.contains(&file.media_id) {
            continue;
        }
        let Some(path) = super::poster_path(file) else {
            continue;
        };
        match std::fs::read(&path).and_then(|bytes| {
            image::load_from_memory(&bytes)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        }) {
            Ok(poster) => {
                seen_media_ids.insert(file.media_id);
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

fn render_library_cover(
    posters: &[DynamicImage],
    options: &CoverOptions,
) -> Result<Vec<u8>, String> {
    let font = include_bytes!("../../../../cover-generator/assets/fonts/chaohei.ttf");
    let generated = generate_cover(posters, options, font, None)
        .map_err(|error| format!("cover.generate: {error}"))?;
    let mut jpeg = Vec::new();
    generated
        .write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
        .map_err(|error| format!("cover.encode: {error}"))?;
    Ok(jpeg)
}
