//! Direct Playback plans and discovered track delivery choices.
use super::*;

pub(super) fn direct_decision(
    file: &domain::LibraryFile,
    body: &Value,
    tracks: Option<&library::Tracks>,
) -> Value {
    let failed_tiers = body["failed_tiers"].as_array();
    let tier_0_failed = failed_tiers.is_some_and(|arr| arr.iter().any(|v| v.as_i64() == Some(0)));
    if tier_0_failed {
        return json!({
            "outcome": "rejected",
            "tier": null,
            "file_id": null,
            "container": null,
            "video": null,
            "audio": null,
            "audio_tracks": [],
            "subtitles": [],
            "degraded_from": null,
            "cost_hint": null,
            "can_self_enable": false,
            "setting_namespace": null,
            "setting_key": null,
            "reason": "当前浏览器或设备不支持直接解码该视频格式（流媒体 STRM 资源建议直接串流或使用支持该编码的播放客户端）",
            "suggestion": "建议使用 Infuse / VidHub / 外部播放器播放该 STRM 直链，或在设置中配置转码服务",
            "capability": body["capability"],
        });
    }
    let path = &file.path;
    let is_strm = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("strm"));

    // 对于 STRM 虚拟流，前端直接以直通格式播放（统一按通用容器 mp4 处理）
    let container = if is_strm {
        "mp4".to_string()
    } else {
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("mkv")
            .to_string()
    };
    let (audio_tracks, subtitles) = direct_tracks(file, tracks);
    json!({
        "outcome": "plan",
        "tier": 0,
        "file_id": file.file_id.to_string(),
        "container": container,
        "video": { "action": "copy", "codec": file.codec, "height": null, "tone_map": false, "burn_subtitle": null },
        "audio": { "action": "copy", "track_ref": null, "codec": null, "channels": null, "downmix": false },
        // 直出整份文件时浏览器只放容器默认音轨，请求里的 audio_track 不会生效。
        // 不把探测到的多条轨交给菜单，避免用户点了之后白重启、仍听到默认轨。
        "audio_tracks": if audio_tracks.len() < 2 { audio_tracks } else { Vec::<Value>::new() },
        "subtitles": subtitles,
        "degraded_from": null,
        "cost_hint": null,
        "can_self_enable": false,
        "setting_namespace": null,
        "setting_key": null,
        "reason": "",
        "suggestion": null,
        "capability": body["capability"],
    })
}

fn direct_tracks(
    file: &domain::LibraryFile,
    tracks: Option<&library::Tracks>,
) -> (Vec<Value>, Vec<Value>) {
    match tracks {
        Some(t) => {
            let a = t
                .audio
                .iter()
                .enumerate()
                .map(|(idx, a)| {
                    let stream_idx = a.stream_index.unwrap_or(idx as u32);
                    json!({
                        "index": stream_idx,
                        "ref": format!("embedded:{stream_idx}"),
                        "codec": a.codec,
                        "language": a.language,
                        "title": a.title,
                        "channels": a.channels,
                        "is_default": a.is_default,
                    })
                })
                .collect();
            let s = t
                .subtitles
                .iter()
                .enumerate()
                .map(|(idx, s)| {
                    let stream_idx = library::subtitle_index(t, idx);
                    let track_ref = if s.is_external {
                        let fname = s
                            .path
                            .as_ref()
                            .and_then(|p| std::path::Path::new(p).file_name())
                            .and_then(|n| n.to_str())
                            .unwrap_or("subtitle");
                        format!("external:{fname}")
                    } else {
                        format!("embedded:{stream_idx}")
                    };
                    let kind = match s.codec.as_deref().unwrap_or("srt").to_lowercase().as_str() {
                        "ass" | "ssa" => "ass",
                        "pgs" | "sup" => "pgs",
                        _ => "vtt",
                    };
                    json!({
                        "index": stream_idx,
                        "track_ref": track_ref,
                        "kind": kind,
                        "codec": s.codec,
                        "language": s.language,
                        "title": s.title,
                        "is_default": s.is_default,
                        "forced": s.forced,
                        "is_external": s.is_external,
                        "delivery_url": format!("/api/v1/playback/subtitles/{}/{}", file.file_id, stream_idx),
                    })
                })
                .collect();
            (a, s)
        }
        None => (Vec::new(), Vec::new()),
    }
}
