//! `<streamdetails>` 的生成与写回。

use std::path::Path;

use super::super::parse::{is_supported_root, name_is};
use super::elements::root_close_start;
use super::xml_escape;

/// 把 ffprobe 探测到的媒体流信息（<fileinfo><streamdetails>）合并写入已有 NFO。
///
/// 探测是懒加载的，所以这里采用「探测完成后再补写」的语义：
/// - 目标 NFO 不存在 → 跳过（没有可以补的宿主文件）；
/// - 已含 `<fileinfo>`（例如 MoviePilot / TMM 早已生成）→ 跳过，绝不覆盖别人的成果；
pub fn write_streamdetails_into_nfo(
    nfo_path: &Path,
    video: Option<&crate::VideoTrack>,
    audio: &[crate::AudioTrack],
    subtitles: &[crate::SubtitleTrack],
) -> std::io::Result<()> {
    let body = match std::fs::read_to_string(nfo_path) {
        Ok(body) => body,
        Err(_) => return Ok(()),
    };
    let Ok(document) = roxmltree::Document::parse(&body) else {
        return Ok(());
    };
    let root = document.root_element();
    if !is_supported_root(root.tag_name().name())
        || root
            .children()
            .any(|child| child.is_element() && name_is(child.tag_name().name(), "fileinfo"))
    {
        return Ok(());
    }
    let Some(idx) = root_close_start(&body, root) else {
        return Ok(());
    };
    let block = streamdetails_xml(video, audio, subtitles);
    let mut insertion = String::new();
    if idx > 0 && !body[..idx].ends_with('\n') {
        insertion.push('\n');
    }
    insertion.push_str(&block);
    let mut updated = String::with_capacity(body.len() + insertion.len());
    updated.push_str(&body[..idx]);
    updated.push_str(&insertion);
    updated.push_str(&body[idx..]);
    std::fs::write(nfo_path, updated)
}

fn streamdetails_xml(
    video: Option<&crate::VideoTrack>,
    audio: &[crate::AudioTrack],
    subtitles: &[crate::SubtitleTrack],
) -> String {
    let mut block = String::from("<fileinfo>\n  <streamdetails>\n");
    if let Some(v) = video {
        append_video_stream(&mut block, v);
    }
    for t in audio {
        append_audio_stream(&mut block, t);
    }
    for t in subtitles {
        append_subtitle_stream(&mut block, t);
    }
    block.push_str("  </streamdetails>\n</fileinfo>\n");
    block
}

fn append_video_stream(block: &mut String, track: &crate::VideoTrack) {
    block.push_str("    <video>\n");
    if let Some(codec) = &track.codec {
        block.push_str(&stream_field("codec", codec));
    }
    if let Some(value) = track.bit_rate {
        block.push_str(&stream_field("bitrate", &value.to_string()));
    }
    if let Some(value) = track.width {
        block.push_str(&stream_field("width", &value.to_string()));
    }
    if let Some(value) = track.height {
        block.push_str(&stream_field("height", &value.to_string()));
    }
    if let (Some(width), Some(height)) = (track.width, track.height)
        && height > 0
    {
        block.push_str(&stream_field(
            "aspect",
            &format!("{:.2}:1", width as f64 / height as f64),
        ));
    }
    if let Some(value) = track.frame_rate {
        block.push_str(&stream_field("framerate", &format!("{value:.3}")));
    }
    if let Some(value) = track.duration_secs {
        block.push_str(&stream_field("duration", &format!("{value:.0}")));
    }
    block.push_str("    </video>\n");
}

fn append_audio_stream(block: &mut String, track: &crate::AudioTrack) {
    block.push_str("    <audio>\n");
    append_optional_stream_field(block, "codec", track.codec.as_deref());
    if let Some(channels) = track.channels {
        block.push_str(&stream_field("channels", &channels.to_string()));
    }
    append_optional_stream_field(block, "language", track.language.as_deref());
    append_optional_stream_field(block, "samplingrate", track.sample_rate.as_deref());
    block.push_str(&stream_field("default", bool_text(track.is_default)));
    block.push_str("    </audio>\n");
}

fn append_subtitle_stream(block: &mut String, track: &crate::SubtitleTrack) {
    block.push_str("    <subtitle>\n");
    append_optional_stream_field(block, "codec", track.codec.as_deref());
    append_optional_stream_field(block, "language", track.language.as_deref());
    block.push_str(&stream_field("default", bool_text(track.is_default)));
    block.push_str(&stream_field("forced", bool_text(track.forced)));
    block.push_str("    </subtitle>\n");
}

fn append_optional_stream_field(block: &mut String, name: &str, value: Option<&str>) {
    if let Some(value) = value {
        block.push_str(&stream_field(name, value));
    }
}

fn stream_field(name: &str, value: &str) -> String {
    format!("      <{name}>{}</{name}>\n", xml_escape(value))
}

fn bool_text(value: bool) -> &'static str {
    if value { "True" } else { "False" }
}
