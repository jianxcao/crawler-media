//! Brand logo normalization (`variant=brand-logo`) for dark glass UI badges.
//!
//! Removes opaque white-box backgrounds via border flood-fill, trims excessive
//! transparent padding to a tight bounding box, and lifts dark monochrome
//! wordmarks to crisp white while preserving 100% of saturated brand colors.

use std::collections::VecDeque;
use std::io::Cursor;

use image::{ImageFormat, ImageReader, Limits, RgbaImage, imageops};

pub(crate) fn normalize_brand_logo(content_type: &str, bytes: Vec<u8>) -> (String, Vec<u8>) {
    if content_type.to_ascii_lowercase().contains("svg") {
        return (content_type.to_string(), bytes);
    }
    let Ok(mut reader) = ImageReader::new(Cursor::new(&bytes)).with_guessed_format() else {
        return (content_type.to_string(), bytes);
    };
    let mut limits = Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(16 * 1024 * 1024);
    reader.limits(limits);
    let Ok(decoded) = reader.decode() else {
        return (content_type.to_string(), bytes);
    };
    let mut img = decoded.to_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return (content_type.to_string(), bytes);
    }

    strip_light_box_background(&mut img);
    let mut cropped = crop_alpha_bounds(img);
    lift_dark_monochrome(&mut cropped);

    let mut out = Vec::new();
    if cropped
        .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .is_err()
    {
        return (content_type.to_string(), bytes);
    }
    ("image/png".to_string(), out)
}

fn is_corner_white(px: [u8; 4]) -> bool {
    let [r, g, b, a] = px;
    let mn = r.min(g).min(b);
    let mx = r.max(g).max(b);
    a >= 220 && mn >= 180 && (mx - mn) <= 20
}

fn is_light_bg(px: [u8; 4]) -> bool {
    let [r, g, b, a] = px;
    let mn = r.min(g).min(b);
    let mx = r.max(g).max(b);
    a >= 200 && mn >= 175 && (mx - mn) <= 24
}

fn strip_light_box_background(img: &mut RgbaImage) {
    let (w, h) = img.dimensions();
    let corners = [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)];
    let corner_matches = corners
        .iter()
        .filter(|&&(x, y)| is_corner_white(img.get_pixel(x, y).0))
        .count();
    if corner_matches < 3 {
        return;
    }

    let len = (w as usize) * (h as usize);
    let idx_of = |x: u32, y: u32| (y as usize) * (w as usize) + (x as usize);
    let mut visited = vec![false; len];
    let mut queue = VecDeque::new();

    for x in 0..w {
        seed_bg_pixel(img, x, 0, &mut visited, &mut queue);
        seed_bg_pixel(img, x, h - 1, &mut visited, &mut queue);
    }
    for y in 0..h {
        seed_bg_pixel(img, 0, y, &mut visited, &mut queue);
        seed_bg_pixel(img, w - 1, y, &mut visited, &mut queue);
    }

    while let Some((x, y)) = queue.pop_front() {
        img.get_pixel_mut(x, y).0[3] = 0;
        for (nx, ny) in neighbors4(x, y, w, h).into_iter().flatten() {
            let nidx = idx_of(nx, ny);
            if !visited[nidx] && is_light_bg(img.get_pixel(nx, ny).0) {
                visited[nidx] = true;
                queue.push_back((nx, ny));
            }
        }
    }

    feather_background_edges(img, &visited);
}

fn seed_bg_pixel(
    img: &RgbaImage,
    x: u32,
    y: u32,
    visited: &mut [bool],
    queue: &mut VecDeque<(u32, u32)>,
) {
    let w = img.width() as usize;
    let idx = (y as usize) * w + (x as usize);
    if !visited[idx] && is_light_bg(img.get_pixel(x, y).0) {
        visited[idx] = true;
        queue.push_back((x, y));
    }
}

fn feather_background_edges(img: &mut RgbaImage, visited: &[bool]) {
    let (w, h) = img.dimensions();
    let idx_of = |x: u32, y: u32| (y as usize) * (w as usize) + (x as usize);
    for y in 0..h {
        for x in 0..w {
            let idx = idx_of(x, y);
            if visited[idx] {
                continue;
            }
            let touches_bg = neighbors4(x, y, w, h)
                .into_iter()
                .flatten()
                .any(|(nx, ny)| visited[idx_of(nx, ny)]);
            if !touches_bg {
                continue;
            }
            let [r, g, b, a] = img.get_pixel(x, y).0;
            let mn = r.min(g).min(b);
            let mx = r.max(g).max(b);
            if (mx - mn) <= 28 && mn >= 155 {
                let new_a =
                    (245_u32.saturating_sub(mn as u32) * (a as u32) / 90).min(a as u32) as u8;
                img.get_pixel_mut(x, y).0[3] = new_a;
            }
        }
    }
}

fn neighbors4(x: u32, y: u32, w: u32, h: u32) -> [Option<(u32, u32)>; 4] {
    [
        x.checked_sub(1).map(|nx| (nx, y)),
        x.checked_add(1).filter(|&nx| nx < w).map(|nx| (nx, y)),
        y.checked_sub(1).map(|ny| (x, ny)),
        y.checked_add(1).filter(|&ny| ny < h).map(|ny| (x, ny)),
    ]
}

fn crop_alpha_bounds(img: RgbaImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    let mut min_x = w;
    let mut min_y = h;
    let mut max_x = 0_u32;
    let mut max_y = 0_u32;
    let mut found = false;

    for y in 0..h {
        for x in 0..w {
            if img.get_pixel(x, y).0[3] > 16 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
                found = true;
            }
        }
    }

    if found && max_x >= min_x && max_y >= min_y {
        imageops::crop_imm(&img, min_x, min_y, max_x - min_x + 1, max_y - min_y + 1).to_image()
    } else {
        img
    }
}

fn lift_dark_monochrome(img: &mut RgbaImage) {
    for px in img.pixels_mut() {
        let [r, g, b, a] = px.0;
        if a <= 12 {
            continue;
        }
        let mx = r.max(g).max(b);
        let mn = r.min(g).min(b);
        let chroma = mx - mn;
        if chroma >= 26 {
            continue;
        }
        let luma = (299 * (r as u32) + 587 * (g as u32) + 114 * (b as u32)) / 1000;
        if luma < 145 {
            let target = 248_u32.saturating_sub(luma * 88 / 145).clamp(160, 248) as u8;
            px.0 = [target, target, target, a];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_brand_logo;
    use image::{ImageFormat, Rgba, RgbaImage};
    use std::io::Cursor;

    fn encode_png(img: &RgbaImage) -> Vec<u8> {
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
            .unwrap();
        buf
    }

    #[test]
    fn svg_and_invalid_bytes_pass_through_unchanged() {
        let svg = b"<svg></svg>".to_vec();
        let (ct, out) = normalize_brand_logo("image/svg+xml", svg.clone());
        assert_eq!(ct, "image/svg+xml");
        assert_eq!(out, svg);

        let raw = vec![1, 2, 3, 4];
        let (ct, out) = normalize_brand_logo("image/png", raw.clone());
        assert_eq!(ct, "image/png");
        assert_eq!(out, raw);
    }

    #[test]
    fn strips_white_box_crops_padding_preserves_colors_and_lifts_black_text() {
        let mut img = RgbaImage::from_pixel(40, 100, Rgba([255, 255, 255, 255]));
        // Colored icon block (orange) at y = 35..45, x = 8..16
        for y in 35..45 {
            for x in 8..16 {
                img.put_pixel(x, y, Rgba([255, 110, 20, 255]));
            }
        }
        // Dark black wordmark at y = 50..65, x = 18..32
        for y in 50..65 {
            for x in 18..32 {
                img.put_pixel(x, y, Rgba([18, 18, 20, 255]));
            }
        }

        let (ct, out_bytes) = normalize_brand_logo("image/jpeg", encode_png(&img));
        assert_eq!(ct, "image/png");
        let out = image::load_from_memory(&out_bytes).unwrap().to_rgba8();
        assert_eq!(out.dimensions(), (24, 30));
        // Top-right corner inside cropped rect was white background -> now transparent
        assert_eq!(out.get_pixel(23, 0).0[3], 0);
        // Brand orange preserved 100%
        assert_eq!(out.get_pixel(2, 2).0, [255, 110, 20, 255]);
        // Dark wordmark lifted to crisp white
        let lifted = out.get_pixel(15, 20).0;
        assert!(lifted[0] >= 230 && lifted[1] >= 230 && lifted[2] >= 230);
        assert_eq!(lifted[3], 255);
    }
}
