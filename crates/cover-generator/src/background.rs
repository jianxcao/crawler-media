use crate::color::{RgbColor, extract_dominant_color};
use crate::error::CoverError;
use crate::styles::BackgroundOption;
use image::{DynamicImage, GenericImageView, ImageBuffer, Rgba};

pub fn render_background(
    width: u32,
    height: u32,
    sources: &[DynamicImage],
    bg_option: &BackgroundOption,
) -> Result<tiny_skia::Pixmap, CoverError> {
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| CoverError::RenderError("Failed to allocate background pixmap".into()))?;

    match bg_option {
        BackgroundOption::SolidColor { hex_color } => {
            let color = RgbColor::from_hex(hex_color).unwrap_or(RgbColor(30, 35, 45));
            pixmap.fill(color.to_tiny_skia_color(1.0));
        }
        BackgroundOption::Gradient {
            from_hex,
            to_hex,
            angle: _,
        } => {
            let from = RgbColor::from_hex(from_hex).unwrap_or(RgbColor(25, 30, 45));
            let to = RgbColor::from_hex(to_hex).unwrap_or(RgbColor(10, 15, 25));

            // 垂直渐变填充
            let paint = {
                let shader = tiny_skia::LinearGradient::new(
                    tiny_skia::Point::from_xy(0.0, 0.0),
                    tiny_skia::Point::from_xy(0.0, height as f32),
                    vec![
                        tiny_skia::GradientStop::new(0.0, from.to_tiny_skia_color(1.0)),
                        tiny_skia::GradientStop::new(1.0, to.to_tiny_skia_color(1.0)),
                    ],
                    tiny_skia::SpreadMode::Pad,
                    tiny_skia::Transform::identity(),
                )
                .ok_or_else(|| {
                    CoverError::RenderError("Failed to create gradient shader".into())
                })?;
                let mut p = tiny_skia::Paint::default();
                p.shader = shader;
                p
            };
            let rect = tiny_skia::Rect::from_xywh(0.0, 0.0, width as f32, height as f32)
                .ok_or_else(|| CoverError::RenderError("Invalid canvas rect".into()))?;
            pixmap.fill_rect(rect, &paint, tiny_skia::Transform::identity(), None);
        }
        BackgroundOption::AutoExtract {
            blur_radius,
            color_ratio,
        } => {
            let base_color = if let Some(first) = sources.first() {
                extract_dominant_color(first).to_macaron()
            } else {
                RgbColor(100, 120, 160)
            };

            // 如果有来源图，将其缩放铺满作为模糊底板
            if let Some(src) = sources.first() {
                let blurred = create_blurred_backdrop(src, width, height, *blur_radius);
                let bg_color = base_color.darken(0.85);

                // 混合模糊底图与纯色 (1 - color_ratio) * 模糊图 + color_ratio * bg_color
                let w = pixmap.width();
                for (x, y, pixel) in blurred.enumerate_pixels() {
                    let r = pixel[0] as f32 * (1.0 - color_ratio) + bg_color.0 as f32 * color_ratio;
                    let g = pixel[1] as f32 * (1.0 - color_ratio) + bg_color.1 as f32 * color_ratio;
                    let b = pixel[2] as f32 * (1.0 - color_ratio) + bg_color.2 as f32 * color_ratio;
                    let idx = (y * w + x) as usize;
                    if let Some(p) = pixmap.pixels_mut().get_mut(idx) {
                        *p = tiny_skia::ColorU8::from_rgba(r as u8, g as u8, b as u8, 255)
                            .premultiply();
                    }
                }
            } else {
                pixmap.fill(base_color.to_tiny_skia_color(1.0));
            }
        }
        BackgroundOption::CustomImage { darken } => {
            if let Some(src) = sources.first() {
                let fitted = src.resize_exact(width, height, image::imageops::FilterType::Lanczos3);
                let darken_factor = (1.0 - darken).clamp(0.0, 1.0);
                let w = pixmap.width();
                for (x, y, pixel) in fitted.pixels() {
                    let r = (pixel[0] as f32 * darken_factor) as u8;
                    let g = (pixel[1] as f32 * darken_factor) as u8;
                    let b = (pixel[2] as f32 * darken_factor) as u8;
                    let idx = (y * w + x) as usize;
                    if let Some(p) = pixmap.pixels_mut().get_mut(idx) {
                        *p = tiny_skia::ColorU8::from_rgba(r, g, b, 255).premultiply();
                    }
                }
            } else {
                pixmap.fill(tiny_skia::Color::from_rgba8(20, 24, 34, 255));
            }
        }
    }

    Ok(pixmap)
}

fn create_blurred_backdrop(
    src: &DynamicImage,
    target_width: u32,
    target_height: u32,
    blur_radius: f32,
) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    // 降采样以大幅加速高斯模糊
    let down_w = (target_width / 8).max(32);
    let down_h = (target_height / 8).max(32);
    let small = src.resize_exact(down_w, down_h, image::imageops::FilterType::Triangle);
    let blurred = image::imageops::blur(&small, (blur_radius / 8.0).max(1.0));
    let dyn_blurred = DynamicImage::ImageRgba8(blurred);
    dyn_blurred
        .resize_exact(
            target_width,
            target_height,
            image::imageops::FilterType::CatmullRom,
        )
        .to_rgba8()
}
