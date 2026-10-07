use serde_json::{Value, json};

pub fn media_streams_json(tracks: &library::Tracks) -> Vec<Value> {
    media_streams_with_item(tracks, None)
}

pub fn media_streams_with_item(tracks: &library::Tracks, item_id: Option<&str>) -> Vec<Value> {
    let mut streams = Vec::new();
    if let Some(video) = &tracks.video {
        streams.push(json!({
            "Index": video.stream_index.unwrap_or_default(),
            "Type": "Video",
            "Codec": video.codec,
            "Profile": video.profile,
            "Width": video.width,
            "Height": video.height,
            "AspectRatio": video.aspect_ratio,
            "SampleAspectRatio": video.sample_aspect_ratio,
            "PixelFormat": video.pixel_format,
            "BitDepth": video.bit_depth,
            "RealFrameRate": video.frame_rate,
            "AverageFrameRate": video.average_frame_rate,
            "BitRate": video.bit_rate,
            "ColorSpace": video.color_space,
            "ColorTransfer": video.color_transfer,
            "ColorPrimaries": video.color_primaries,
            "FieldOrder": video.field_order,
            "DisplayTitle": display_title(None, None, video.profile.as_deref(), video.codec.as_deref()),
            "IsDefault": video.is_default,
        }));
    }
    for audio in &tracks.audio {
        streams.push(json!({
            "Index": audio.stream_index.unwrap_or(streams.len() as u32),
            "Type": "Audio",
            "Codec": audio.codec,
            "Profile": audio.profile,
            "Title": audio.title,
            "DisplayTitle": display_title(audio.title.as_deref(), audio.language.as_deref(), audio.profile.as_deref(), audio.codec.as_deref()),
            "Channels": audio.channels,
            "ChannelLayout": audio.channel_layout,
            "Language": audio.language,
            "SampleRate": audio.sample_rate.as_deref().and_then(|value| value.parse::<u32>().ok()),
            "BitRate": audio.bit_rate,
            "BitDepth": audio.bits_per_sample,
            "IsDefault": audio.is_default,
            "IsForced": audio.forced,
            "IsExternal": false,
        }));
    }
    for (ordinal, subtitle) in tracks.subtitles.iter().enumerate() {
        let stream_idx = library::subtitle_index(tracks, ordinal);
        let delivery_url = item_id.map(|id| {
            format!(
                "/Videos/{}/Subtitles/{}/Stream.{}",
                id,
                stream_idx,
                subtitle.codec.as_deref().unwrap_or("srt")
            )
        });
        streams.push(json!({
            "Index": stream_idx,
            "Type": "Subtitle",
            "Codec": subtitle.codec,
            "Profile": subtitle.profile,
            "Title": subtitle.title,
            "DisplayTitle": display_title(subtitle.title.as_deref(), subtitle.language.as_deref(), subtitle.profile.as_deref(), subtitle.codec.as_deref()),
            "Language": subtitle.language,
            "BitRate": subtitle.bit_rate,
            "IsExternal": subtitle.is_external,
            "IsDefault": subtitle.is_default,
            "IsForced": subtitle.forced,
            "Path": subtitle.path,
            "DeliveryUrl": delivery_url,
        }));
    }
    streams
}

fn display_title(
    title: Option<&str>,
    language: Option<&str>,
    profile: Option<&str>,
    codec: Option<&str>,
) -> Option<String> {
    title
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            let label = [language, profile, codec]
                .into_iter()
                .flatten()
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            (!label.is_empty()).then_some(label)
        })
}
