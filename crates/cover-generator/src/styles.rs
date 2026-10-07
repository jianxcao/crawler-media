use crate::background::render_background;
use crate::canvas::CanvasUtils;
use crate::error::CoverError;
use crate::text::TextRenderer;
use image::DynamicImage;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum BackgroundOption {
    /// 自动提取主色 + 高斯模糊原图底纹
    AutoExtract {
        #[serde(default = "default_blur")]
        blur_radius: f32,
        #[serde(default = "default_color_ratio")]
        color_ratio: f32,
    },
    /// 纯色底
    SolidColor { hex_color: String },
    /// 线性渐变
    Gradient {
        from_hex: String,
        to_hex: String,
        #[serde(default)]
        angle: f32,
    },
    /// 自定义底图
    CustomImage {
        #[serde(default = "default_darken")]
        darken: f32,
    },
}

fn default_blur() -> f32 {
    50.0
}
fn default_color_ratio() -> f32 {
    0.8
}
fn default_darken() -> f32 {
    0.3
}

impl Default for BackgroundOption {
    fn default() -> Self {
        Self::AutoExtract {
            blur_radius: 50.0,
            color_ratio: 0.8,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CoverStyle {
    /// 经典单图微倾斜卡片（style_static_1）
    MacaronCardSingle,
    /// 多图错落堆叠（style_static_2）
    MultiPosterPile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverOptions {
    pub title_zh: String,
    pub title_en: Option<String>,
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default = "default_style")]
    pub style: CoverStyle,
    #[serde(default)]
    pub background: BackgroundOption,
}

fn default_width() -> u32 {
    1920
}
fn default_height() -> u32 {
    1080
}
fn default_style() -> CoverStyle {
    CoverStyle::MacaronCardSingle
}

pub fn render_cover(
    sources: &[DynamicImage],
    options: &CoverOptions,
    font_bytes_zh: &[u8],
    font_bytes_en: Option<&[u8]>,
) -> Result<DynamicImage, CoverError> {
    if sources.is_empty() {
        return Err(CoverError::EmptySourceImages);
    }

    // 1. 渲染背景
    let mut canvas =
        render_background(options.width, options.height, sources, &options.background)?;

    // 2. 根据排版风格绘制卡片
    match options.style {
        CoverStyle::MacaronCardSingle => {
            // 支持三张海报叠加（多剧融合）；如果只有 1 部剧，底层自动生成半透明色调阴影卡片增强层次感
            let card_size = (options.height as f32 * 0.72) as u32; // 正方形卡片（MoviePilot 经典 70% 高度）
            let center_x = options.width as f32 * 0.72;
            let center_y = options.height as f32 * 0.50;

            // 裁切为正方形卡片并加圆角
            let make_square_card = |img: &DynamicImage| -> Result<tiny_skia::Pixmap, CoverError> {
                let (w, h) = (img.width(), img.height());
                let min_side = w.min(h);
                let cropped =
                    img.crop_imm((w - min_side) / 2, (h - min_side) / 2, min_side, min_side);
                let resized = cropped.resize_exact(
                    card_size,
                    card_size,
                    image::imageops::FilterType::Lanczos3,
                );
                let raw_card = CanvasUtils::image_to_pixmap(&resized)?;
                CanvasUtils::clip_rounded_rect(&raw_card, card_size as f32 * 0.16)
            };

            // 如果有至少 3 部作品海报，取前 3 部的海报；否则复用主海报
            let p0 = &sources[0];
            let p1 = sources.get(1).unwrap_or(p0);
            let p2 = sources.get(2).unwrap_or(p1);

            let card0 = make_square_card(p0)?; // 顶层主卡
            let card1 = make_square_card(p1)?; // 中间层卡
            let card2 = make_square_card(p2)?; // 底层卡

            // 按照 MoviePilot 旋转与层叠顺序：底层 (36°)、中间层 (18°)、顶层 (0°)
            CanvasUtils::draw_card_with_shadow(
                &mut canvas,
                &card2,
                center_x,
                center_y,
                36.0,
                16.0,
                0.35,
            )?;

            CanvasUtils::draw_card_with_shadow(
                &mut canvas,
                &card1,
                center_x,
                center_y,
                18.0,
                18.0,
                0.40,
            )?;

            CanvasUtils::draw_card_with_shadow(
                &mut canvas,
                &card0,
                center_x,
                center_y,
                0.0,
                24.0,
                0.55,
            )?;
        }
        CoverStyle::MultiPosterPile => {
            // 最多取 3 张海报错落排列
            let count = sources.len().min(3);
            let card_w = (options.width as f32 * 0.26) as u32;
            let card_h = (card_w as f32 * 1.45) as u32;

            let offsets = [(0.60, 0.55, -12.0), (0.78, 0.56, 10.0), (0.69, 0.48, -2.0)];

            for i in 0..count {
                let img = &sources[i];
                let resized =
                    img.resize_exact(card_w, card_h, image::imageops::FilterType::Lanczos3);
                let raw_card = CanvasUtils::image_to_pixmap(&resized)?;
                let rounded_card = CanvasUtils::clip_rounded_rect(&raw_card, 22.0)?;

                let (x_ratio, y_ratio, angle) = offsets[i % offsets.len()];
                CanvasUtils::draw_card_with_shadow(
                    &mut canvas,
                    &rounded_card,
                    options.width as f32 * x_ratio,
                    options.height as f32 * y_ratio,
                    angle,
                    20.0,
                    0.4,
                )?;
            }
        }
    }

    // 3. 绘制文字排版（在左侧区域宽度的四分之一处水平垂直居中）
    let left_center_x = options.width as f32 * 0.28;
    let left_center_y = options.height as f32 * 0.50;
    let zh_size = options.height as f32 * 0.155; // 醒目大号粗体标题
    let en_size = zh_size * 0.38;

    TextRenderer::draw_text_centered(
        &mut canvas,
        &options.title_zh,
        options.title_en.as_deref(),
        font_bytes_zh,
        font_bytes_en,
        left_center_x,
        left_center_y,
        zh_size,
        en_size,
    )?;

    // 4. 将最终画布转回 DynamicImage (RgbImage 用于 JPEG 输出兼容性)
    let rgba_buffer =
        image::ImageBuffer::from_raw(canvas.width(), canvas.height(), canvas.data().to_vec())
            .ok_or_else(|| {
                CoverError::RenderError("Failed to convert pixmap to image buffer".into())
            })?;
    let dyn_rgba = DynamicImage::ImageRgba8(rgba_buffer);
    Ok(DynamicImage::ImageRgb8(dyn_rgba.to_rgb8()))
}
