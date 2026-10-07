use cover_generator::{
    BackgroundOption, CoverOptions, CoverStyle, color::RgbColor, generate_cover,
};
use image::{DynamicImage, Rgba, RgbaImage};

#[test]
fn test_color_conversions() {
    let hex = "#ff8040";
    let color = RgbColor::from_hex(hex).unwrap();
    assert_eq!(color.0, 255);
    assert_eq!(color.1, 128);
    assert_eq!(color.2, 64);

    let short_hex = "#fff";
    let white = RgbColor::from_hex(short_hex).unwrap();
    assert_eq!(white, RgbColor(255, 255, 255));

    // 非 ASCII 字符绝不可引起切片 panic
    assert!(RgbColor::from_hex("#0你00").is_none());
    assert!(RgbColor::from_hex("你好呀").is_none());

    let macaron = color.to_macaron();
    assert!(macaron.0 > 0);

    let darkened = color.darken(0.5);
    assert!(darkened.0 <= color.0);
}

#[test]
fn test_generate_cover_solid_and_auto() {
    // 创建一个测试海报 (400x600)
    let mut poster = RgbaImage::new(400, 600);
    for (x, y, pixel) in poster.enumerate_pixels_mut() {
        let r = ((x * 255) / 400) as u8;
        let g = ((y * 255) / 600) as u8;
        let b = 150;
        *pixel = Rgba([r, g, b, 255]);
    }
    let poster_img = DynamicImage::ImageRgba8(poster);

    // 使用系统自带的一个有效 TTF 字体，或者内置测试用的简单字体
    // macOS 上必有 /System/Library/Fonts/Helvetica.ttc 或 Arial
    let font_bytes = std::fs::read("/System/Library/Fonts/Helvetica.ttc")
        .or_else(|_| std::fs::read("/System/Library/Fonts/Supplemental/Arial.ttf"))
        .or_else(|_| std::fs::read("/Library/Fonts/Arial.ttf"))
        .unwrap_or_else(|_| vec![]);

    if font_bytes.is_empty() {
        eprintln!("Skipping full cover render due to no local font in test runner");
        return;
    }

    let options = CoverOptions {
        title_zh: "华语电影".into(),
        title_en: Some("CHINESE MOVIES".into()),
        width: 960,
        height: 540,
        style: CoverStyle::MacaronCardSingle,
        background: BackgroundOption::AutoExtract {
            blur_radius: 30.0,
            color_ratio: 0.7,
        },
    };

    let result = generate_cover(&[poster_img.clone()], &options, &font_bytes, None);
    assert!(
        result.is_ok(),
        "Failed to generate cover: {:?}",
        result.err()
    );
    let out = result.unwrap();
    assert_eq!(out.width(), 960);
    assert_eq!(out.height(), 540);

    // 测试渐变模式与多图堆叠
    let options_multi = CoverOptions {
        title_zh: "科幻合集".into(),
        title_en: Some("SCI-FI COLLECTION".into()),
        width: 960,
        height: 540,
        style: CoverStyle::MultiPosterPile,
        background: BackgroundOption::Gradient {
            from_hex: "#1f2438".into(),
            to_hex: "#0c0d14".into(),
            angle: 0.0,
        },
    };

    let result_multi = generate_cover(
        &[poster_img.clone(), poster_img.clone(), poster_img],
        &options_multi,
        &font_bytes,
        None,
    );
    assert!(
        result_multi.is_ok(),
        "Failed to generate multi-pile cover: {:?}",
        result_multi.err()
    );
}
