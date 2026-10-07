const RESOLUTIONS: &[&str] = &["2160p", "1080p", "720p", "480p", "4k", "uhd"];
const SOURCES: &[&str] = &[
    "remux", "bluray", "blu-ray", "web-dl", "webdl", "webrip", "hdtv", "dvdrip", "cam", "ts",
];
const CODECS: &[&str] = &[
    "x265", "x264", "h.265", "h.264", "h265", "h264", "hevc", "avc", "av1", "mpeg2", "vc-1", "vc1",
];
const HDR: &[&str] = &["hdr10+", "hdr10", "hdr", "dv", "dovi", "dolbyvision"];

/// 字幕语言标记 → 规范化 BCP 47（zh = 中字不分简繁；zh-Hans/zh-Hant 细分）。
pub(super) fn parse_subtitle_language(token: &str) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    match lower.as_str() {
        "chs" => Some("zh-Hans".into()),
        "cht" => Some("zh-Hant".into()),
        "chs&cht" | "cht&chs" | "简繁" | "简繁中字" | "中字" | "中文字幕" | "双语字幕"
        | "官方中字" => Some("zh".into()),
        "简" => Some("zh-Hans".into()),
        "繁" => Some("zh-Hant".into()),
        "english" | "eng" | "en" => Some("en".into()),
        "日字" => Some("ja".into()),
        "韩字" => Some("ko".into()),
        _ => None,
    }
}

/// 音轨语言标记 → 规范化 BCP 47（cmn=国语、yue=粤语）。
pub(super) fn parse_audio_language(token: &str) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    match lower.as_str() {
        "国语" | "mandarin" | "cmn" => Some("cmn".into()),
        "粤语" | "cantonese" | "yue" => Some("yue".into()),
        "english" | "eng" | "en" => Some("en".into()),
        "日语" | "japanese" | "ja" => Some("ja".into()),
        "韩语" | "korean" | "ko" => Some("ko".into()),
        _ => None,
    }
}

pub(super) fn parse_year(token: &str) -> Option<u16> {
    if token.len() != 4 {
        return None;
    }
    let year: u16 = token.parse().ok()?;
    (1900..=2100).contains(&year).then_some(year)
}

pub(super) fn parse_resolution(token: &str) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    if RESOLUTIONS.contains(&lower.as_str()) {
        Some(if lower == "4k" || lower == "uhd" {
            "2160p".into()
        } else {
            lower
        })
    } else {
        None
    }
}

pub(super) fn parse_source(token: &str) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    SOURCES.iter().find(|s| lower == **s).map(|s| {
        if *s == "remux" {
            "Remux".into()
        } else if *s == "blu-ray" {
            "BluRay".into()
        } else if *s == "webdl" {
            "WEB-DL".into()
        } else if *s == "cam" {
            "CAM".into()
        } else if lower == "bluray" {
            "BluRay".into()
        } else {
            token.to_string()
        }
    })
}

pub(super) fn parse_codec(token: &str) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    CODECS
        .iter()
        .find(|c| lower == **c)
        .map(|_| token.to_string())
}

pub(super) fn parse_hdr(token: &str) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    HDR.iter().find(|h| lower == **h).map(|_| token.to_string())
}
