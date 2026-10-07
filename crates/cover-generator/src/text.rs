use crate::error::CoverError;
use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use tiny_skia::{Color, Pixmap};

/// 文字渲染工具（中文主标题 + 英文副标题）
pub struct TextRenderer;

impl TextRenderer {
    /// 测量单行文字的像素宽度
    pub fn measure_line<F: Font>(text: &str, font: &F, size: f32) -> f32 {
        let scaled_font = font.as_scaled(PxScale::from(size));
        let mut width = 0.0;
        for ch in text.chars() {
            width += scaled_font.h_advance(scaled_font.glyph_id(ch));
        }
        width
    }

    /// 在指定区域居中绘制中英文标题
    pub fn draw_text_centered(
        pixmap: &mut Pixmap,
        title_zh: &str,
        title_en: Option<&str>,
        font_bytes_zh: &[u8],
        font_bytes_en: Option<&[u8]>,
        center_x: f32,
        center_y: f32,
        zh_size: f32,
        en_size: f32,
    ) -> Result<(), CoverError> {
        let font_zh = FontRef::try_from_slice(font_bytes_zh)
            .map_err(|e| CoverError::FontError(e.to_string()))?;

        let has_en = title_en.map(|s| !s.trim().is_empty()).unwrap_or(false);
        let spacing = if has_en { zh_size * 0.28 } else { 0.0 };

        // 使用真实 ascent 计算基线：
        // ab_glyph 传入的 y 是 baseline (基线) 的 Y 坐标！
        // 传入 y 时，字形实际上绘制在 [y - ascent, y - descent] 之间！
        let scaled_zh = font_zh.as_scaled(PxScale::from(zh_size));
        let zh_ascent = scaled_zh.ascent();
        let zh_cap_height = zh_ascent * 0.88; // 汉字字腹高度约为 0.88 * ascent

        let (total_content_height, zh_baseline_offset) = if has_en {
            let total = zh_cap_height + spacing + en_size * 0.8;
            (total, zh_ascent)
        } else {
            (zh_cap_height, zh_ascent)
        };

        // 真实的视觉中心：顶部到底部跨越 total_content_height
        // 顶边 top_y = center_y - total_content_height / 2
        // 中文 baseline_y = top_y + zh_baseline_offset
        let top_y = center_y - (total_content_height / 2.0);
        let zh_baseline_y = top_y + zh_baseline_offset;

        // 1. 绘制中文主标题（水平居中 + 加粗阴影描边）
        let zh_width = Self::measure_line(title_zh, &font_zh, zh_size);
        let zh_x = center_x - (zh_width / 2.0);

        Self::draw_bold_line(
            pixmap,
            title_zh,
            &font_zh,
            zh_x,
            zh_baseline_y,
            zh_size,
            Color::from_rgba8(255, 255, 255, 245),
        )?;

        // 2. 绘制英文副标题（如果提供）
        if let Some(en) = title_en {
            if !en.trim().is_empty() {
                let en_top_y = top_y + zh_cap_height + spacing;
                if let Some(en_bytes) = font_bytes_en {
                    if let Ok(font_en) = FontRef::try_from_slice(en_bytes) {
                        let scaled_en = font_en.as_scaled(PxScale::from(en_size));
                        let en_baseline_y = en_top_y + scaled_en.ascent();
                        let en_width = Self::measure_line(en, &font_en, en_size);
                        let en_x = center_x - (en_width / 2.0);
                        Self::draw_bold_line(
                            pixmap,
                            en,
                            &font_en,
                            en_x,
                            en_baseline_y,
                            en_size,
                            Color::from_rgba8(255, 255, 255, 190),
                        )?;
                    } else {
                        let en_baseline_y = en_top_y + scaled_zh.ascent() * (en_size / zh_size);
                        let en_width = Self::measure_line(en, &font_zh, en_size);
                        let en_x = center_x - (en_width / 2.0);
                        Self::draw_bold_line(
                            pixmap,
                            en,
                            &font_zh,
                            en_x,
                            en_baseline_y,
                            en_size,
                            Color::from_rgba8(255, 255, 255, 190),
                        )?;
                    }
                } else {
                    let en_baseline_y = en_top_y + scaled_zh.ascent() * (en_size / zh_size);
                    let en_width = Self::measure_line(en, &font_zh, en_size);
                    let en_x = center_x - (en_width / 2.0);
                    Self::draw_bold_line(
                        pixmap,
                        en,
                        &font_zh,
                        en_x,
                        en_baseline_y,
                        en_size,
                        Color::from_rgba8(255, 255, 255, 190),
                    )?;
                }
            }
        }

        Ok(())
    }

    /// 绘制加粗并带层次阴影的文字
    fn draw_bold_line<F: Font>(
        pixmap: &mut Pixmap,
        text: &str,
        font: &F,
        x: f32,
        y: f32,
        size: f32,
        color: Color,
    ) -> Result<(), CoverError> {
        let scaled_font = font.as_scaled(PxScale::from(size));
        let mut cursor_x = x;
        let w = pixmap.width();
        let h = pixmap.height();

        for ch in text.chars() {
            let mut glyph = scaled_font.scaled_glyph(ch);
            glyph.position = ab_glyph::point(cursor_x, y);
            let advance = scaled_font.h_advance(scaled_font.glyph_id(ch));

            if let Some(outlined) = font.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();

                // 1. 柔和深色阴影层 (单次偏移已足够深色对比，减少循环避免卡顿)
                outlined.draw(|px, py, coverage| {
                    let target_x = bounds.min.x as i32 + px as i32 + 4;
                    let target_y = bounds.min.y as i32 + py as i32 + 6;
                    if target_x >= 0 && target_x < w as i32 && target_y >= 0 && target_y < h as i32
                    {
                        let idx = (target_y as u32 * w + target_x as u32) as usize;
                        if let Some(pixel) = pixmap.pixels_mut().get_mut(idx) {
                            let alpha = (coverage * 0.55 * 255.0) as u8;
                            let shadow_c =
                                tiny_skia::ColorU8::from_rgba(0, 0, 0, alpha).premultiply();
                            *pixel = blend_pixel(*pixel, shadow_c);
                        }
                    }
                });

                // 2. 模拟粗体 (只向右下各外扩 1px)
                for (ox, oy) in [(0, 0), (1, 1)] {
                    outlined.draw(|px, py, coverage| {
                        let target_x = bounds.min.x as i32 + px as i32 + ox;
                        let target_y = bounds.min.y as i32 + py as i32 + oy;
                        if target_x >= 0
                            && target_x < w as i32
                            && target_y >= 0
                            && target_y < h as i32
                        {
                            let idx = (target_y as u32 * w + target_x as u32) as usize;
                            if let Some(pixel) = pixmap.pixels_mut().get_mut(idx) {
                                let r = (color.red() * 255.0) as u8;
                                let g = (color.green() * 255.0) as u8;
                                let b = (color.blue() * 255.0) as u8;
                                let alpha = (color.alpha() * coverage * 255.0) as u8;
                                let glyph_c =
                                    tiny_skia::ColorU8::from_rgba(r, g, b, alpha).premultiply();
                                *pixel = blend_pixel(*pixel, glyph_c);
                            }
                        }
                    });
                }
            }

            cursor_x += advance;
        }

        Ok(())
    }
}

#[inline]
fn blend_pixel(
    dst: tiny_skia::PremultipliedColorU8,
    src: tiny_skia::PremultipliedColorU8,
) -> tiny_skia::PremultipliedColorU8 {
    let src_a = src.alpha() as u32;
    let inv_a = 255 - src_a;

    let r = src.red() as u32 + (dst.red() as u32 * inv_a) / 255;
    let g = src.green() as u32 + (dst.green() as u32 * inv_a) / 255;
    let b = src.blue() as u32 + (dst.blue() as u32 * inv_a) / 255;
    let a = src_a + (dst.alpha() as u32 * inv_a) / 255;

    tiny_skia::PremultipliedColorU8::from_rgba(
        r.min(255) as u8,
        g.min(255) as u8,
        b.min(255) as u8,
        a.min(255) as u8,
    )
    .unwrap_or(dst)
}
