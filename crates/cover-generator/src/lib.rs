pub mod background;
pub mod canvas;
pub mod color;
mod error;
pub mod styles;
pub mod text;

pub use error::CoverError;
pub use image::DynamicImage;
pub use styles::{BackgroundOption, CoverOptions, CoverStyle};

/// 主入口：根据提供的来源图片列表（海报/剧照）与配置，生成媒体库封面图
pub fn generate_cover(
    sources: &[DynamicImage],
    options: &CoverOptions,
    font_bytes_zh: &[u8],
    font_bytes_en: Option<&[u8]>,
) -> Result<DynamicImage, CoverError> {
    styles::render_cover(sources, options, font_bytes_zh, font_bytes_en)
}
