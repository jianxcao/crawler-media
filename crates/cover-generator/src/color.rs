use image::{GenericImageView, Rgba};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RgbColor(pub u8, pub u8, pub u8);

impl RgbColor {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self(r, g, b)
    }

    pub fn to_tiny_skia_color(self, alpha: f32) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba(
            self.0 as f32 / 255.0,
            self.1 as f32 / 255.0,
            self.2 as f32 / 255.0,
            alpha,
        )
        .unwrap_or(tiny_skia::Color::BLACK)
    }

    pub fn from_hex(hex: &str) -> Option<Self> {
        let hex = hex.trim_start_matches('#');
        if !hex.is_ascii() {
            return None;
        }
        if hex.len() == 3 {
            let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
            return Some(Self(r, g, b));
        }
        if hex.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        Some(Self(r, g, b))
    }

    pub fn to_hsv(self) -> (f32, f32, f32) {
        let r = self.0 as f32 / 255.0;
        let g = self.1 as f32 / 255.0;
        let b = self.2 as f32 / 255.0;

        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;

        let h = if delta == 0.0 {
            0.0
        } else if max == r {
            60.0 * (((g - b) / delta).rem_euclid(6.0))
        } else if max == g {
            60.0 * (((b - r) / delta) + 2.0)
        } else {
            60.0 * (((r - g) / delta) + 4.0)
        };

        let s = if max == 0.0 { 0.0 } else { delta / max };
        let v = max;

        (h, s, v)
    }

    pub fn from_hsv(h: f32, s: f32, v: f32) -> Self {
        let c = v * s;
        let x = c * (1.0 - (((h / 60.0) % 2.0) - 1.0).abs());
        let m = v - c;

        let (r1, g1, b1) = if (0.0..60.0).contains(&h) {
            (c, x, 0.0)
        } else if (60.0..120.0).contains(&h) {
            (x, c, 0.0)
        } else if (120.0..180.0).contains(&h) {
            (0.0, c, x)
        } else if (180.0..240.0).contains(&h) {
            (0.0, x, c)
        } else if (240.0..300.0).contains(&h) {
            (x, 0.0, c)
        } else {
            (c, 0.0, x)
        };

        Self(
            ((r1 + m) * 255.0).clamp(0.0, 255.0) as u8,
            ((g1 + m) * 255.0).clamp(0.0, 255.0) as u8,
            ((b1 + m) * 255.0).clamp(0.0, 255.0) as u8,
        )
    }

    /// 转换为优雅柔和的马卡龙色调
    pub fn to_macaron(self) -> Self {
        let (h, s, v) = self.to_hsv();
        let target_s = s.clamp(0.35, 0.65);
        let target_v = v.clamp(0.65, 0.85);
        Self::from_hsv(h, target_s, target_v)
    }

    /// 压暗色彩（用于深色模式或文本/阴影对比）
    pub fn darken(self, factor: f32) -> Self {
        let (h, s, v) = self.to_hsv();
        Self::from_hsv(h, s, (v * factor).clamp(0.0, 1.0))
    }
}

/// 快速提取图像主色（基于采样直方图分析，过滤掉黑白灰）
pub fn extract_dominant_color(img: &image::DynamicImage) -> RgbColor {
    let (width, height) = img.dimensions();
    if width == 0 || height == 0 {
        return RgbColor(50, 60, 80);
    }

    let step_x = (width / 50).max(1);
    let step_y = (height / 50).max(1);

    let mut r_acc: u64 = 0;
    let mut g_acc: u64 = 0;
    let mut b_acc: u64 = 0;
    let mut count: u64 = 0;

    for y in (0..height).step_by(step_y as usize) {
        for x in (0..width).step_by(step_x as usize) {
            let Rgba([r, g, b, a]) = img.get_pixel(x, y);
            if a < 128 {
                continue;
            }
            // 过滤极黑极白与无色灰
            let max_c = r.max(g).max(b);
            let min_c = r.min(g).min(b);
            if max_c < 30 || min_c > 235 || (max_c - min_c) < 18 {
                continue;
            }
            r_acc += r as u64;
            g_acc += g as u64;
            b_acc += b as u64;
            count += 1;
        }
    }

    if count == 0 {
        RgbColor(80, 100, 140)
    } else {
        RgbColor(
            (r_acc / count) as u8,
            (g_acc / count) as u8,
            (b_acc / count) as u8,
        )
    }
}
