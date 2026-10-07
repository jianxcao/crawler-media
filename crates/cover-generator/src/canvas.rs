use crate::error::CoverError;
use image::DynamicImage;
use tiny_skia::{Paint, PathBuilder, Pixmap, PixmapPaint, Transform};

/// 2D 绘图与卡片排版核心工具
pub struct CanvasUtils;

impl CanvasUtils {
    /// 将 DynamicImage 转成 tiny-skia 的 Pixmap (Premultiplied RGBA)
    pub fn image_to_pixmap(img: &DynamicImage) -> Result<Pixmap, CoverError> {
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        let mut pixmap = Pixmap::new(w, h)
            .ok_or_else(|| CoverError::RenderError("Failed to create pixmap from image".into()))?;

        for (x, y, pixel) in rgba.enumerate_pixels() {
            let idx = (y * w + x) as usize;
            if let Some(p) = pixmap.pixels_mut().get_mut(idx) {
                *p = tiny_skia::ColorU8::from_rgba(pixel[0], pixel[1], pixel[2], pixel[3])
                    .premultiply();
            }
        }
        Ok(pixmap)
    }

    /// 给图片切圆角
    pub fn clip_rounded_rect(src: &Pixmap, radius: f32) -> Result<Pixmap, CoverError> {
        let w = src.width() as f32;
        let h = src.height() as f32;
        let mut dest = Pixmap::new(src.width(), src.height())
            .ok_or_else(|| CoverError::RenderError("Failed to allocate clipped pixmap".into()))?;

        let mut pb = PathBuilder::new();
        // 构造圆角矩形
        pb.move_to(radius, 0.0);
        pb.line_to(w - radius, 0.0);
        pb.quad_to(w, 0.0, w, radius);
        pb.line_to(w, h - radius);
        pb.quad_to(w, h, w - radius, h);
        pb.line_to(radius, h);
        pb.quad_to(0.0, h, 0.0, h - radius);
        pb.line_to(0.0, radius);
        pb.quad_to(0.0, 0.0, radius, 0.0);
        pb.close();

        let path = pb
            .finish()
            .ok_or_else(|| CoverError::RenderError("Failed to build round rect".into()))?;

        // 用遮罩将源图片画到带圆角的画布上
        let mut mask = tiny_skia::Mask::new(src.width(), src.height())
            .ok_or_else(|| CoverError::RenderError("Failed to create mask".into()))?;
        mask.fill_path(
            &path,
            tiny_skia::FillRule::Winding,
            true,
            Transform::identity(),
        );

        let mut paint = PixmapPaint::default();
        paint.quality = tiny_skia::FilterQuality::Bicubic;
        dest.draw_pixmap(
            0,
            0,
            src.as_ref(),
            &paint,
            Transform::identity(),
            Some(&mask),
        );

        Ok(dest)
    }

    /// 在底板上绘制带有阴影、旋转角度和缩放的卡片
    pub fn draw_card_with_shadow(
        dest: &mut Pixmap,
        card: &Pixmap,
        center_x: f32,
        center_y: f32,
        rotation_deg: f32,
        shadow_blur: f32,
        shadow_opacity: f32,
    ) -> Result<(), CoverError> {
        let card_w = card.width() as f32;
        let card_h = card.height() as f32;

        // 1. 快速绘制阴影（降采样做模糊，避免在 800x800 大图上跑大半径高斯模糊耗尽 CPU）
        let shadow_offset_x = 12.0;
        let shadow_offset_y = 18.0;

        let sm_w = (card.width() / 4).max(16);
        let sm_h = (card.height() / 4).max(16);
        let mut shadow_map = Pixmap::new(sm_w + 10, sm_h + 10)
            .ok_or_else(|| CoverError::RenderError("Failed to create shadow map".into()))?;

        // 阴影底色
        let mut shadow_paint = Paint::default();
        shadow_paint.set_color_rgba8(0, 0, 0, (shadow_opacity * 255.0) as u8);
        let shadow_rect = tiny_skia::Rect::from_xywh(5.0, 5.0, sm_w as f32, sm_h as f32)
            .ok_or_else(|| CoverError::RenderError("Invalid shadow rect".into()))?;
        shadow_map.fill_rect(shadow_rect, &shadow_paint, Transform::identity(), None);

        // 小图模糊极快 (半径 2~3 像素相当于大图 12~16 像素，计算量减少 16 倍)
        let dyn_shadow = image::DynamicImage::ImageRgba8(
            image::ImageBuffer::from_raw(
                shadow_map.width(),
                shadow_map.height(),
                shadow_map.data().to_vec(),
            )
            .ok_or_else(|| CoverError::RenderError("Failed shadow buffer".into()))?,
        );
        let blurred_shadow = dyn_shadow.blur((shadow_blur / 4.0).max(1.0));
        let upscaled_shadow = blurred_shadow.resize_exact(
            card.width() + 40,
            card.height() + 40,
            image::imageops::FilterType::Triangle,
        );
        let blurred_pixmap = Self::image_to_pixmap(&upscaled_shadow)?;

        let shadow_transform = Transform::identity()
            .post_translate(-((card_w + 40.0) / 2.0), -((card_h + 40.0) / 2.0))
            .post_rotate(rotation_deg)
            .post_translate(center_x + shadow_offset_x, center_y + shadow_offset_y);

        let mut pixmap_paint = PixmapPaint::default();
        pixmap_paint.quality = tiny_skia::FilterQuality::Bicubic;
        dest.draw_pixmap(
            0,
            0,
            blurred_pixmap.as_ref(),
            &pixmap_paint,
            shadow_transform,
            None,
        );

        // 2. 绘制卡片本体
        let card_transform = Transform::identity()
            .post_translate(-(card_w / 2.0), -(card_h / 2.0))
            .post_rotate(rotation_deg)
            .post_translate(center_x, center_y);

        dest.draw_pixmap(0, 0, card.as_ref(), &pixmap_paint, card_transform, None);

        Ok(())
    }
}
