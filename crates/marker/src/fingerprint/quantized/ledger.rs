use super::super::CaptureSourceFacts;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(super) struct Proof {
    pub rate: u32,
    pub adts: Vec<u8>,
    pub packets: u64,
    pub anchors: u64,
    pub skip: u64,
    pub discard: u64,
    pub phase_min: i128,
    pub phase_max: i128,
    pub payload_sha256: String,
}
impl Proof {
    // Phase has units of native sample * milliseconds; divide by the native rate for ms.
    pub fn origin_bounds_us(&self) -> (i128, i128) {
        (self.phase_min * 1000, self.phase_max * 1000)
    }
}

pub(super) fn prepare(bytes: &[u8], source: &CaptureSourceFacts) -> Result<Proof, String> {
    let json: Value = serde_json::from_slice(bytes).map_err(|_| "incomplete packet JSON")?;
    let stream = json["streams"]
        .as_array()
        .filter(|s| s.len() == 1)
        .and_then(|s| s.first())
        .ok_or("audio inventory changed")?;
    let rate = validate_stream(&json, stream, source)?;
    let rate_scale = i128::from(rate);
    let sequence = json["packets_and_frames"]
        .as_array()
        .ok_or("missing packet/frame observations")?;
    let mut packets = Vec::new();
    let mut frames = Vec::new();
    for value in sequence {
        match value["type"].as_str() {
            Some("packet") => packets.push(value),
            Some("frame") => frames.push((value, packets.len())),
            _ => return Err("missing observation type".into()),
        }
    }
    if !(3..=16_384).contains(&packets.len()) {
        return Err("packet proof outside bounded capacity".into());
    }
    validate_frames(&packets, &frames, source, stream)?;
    let mut proof = Proof {
        rate,
        adts: Vec::new(),
        packets: packets.len() as u64,
        anchors: 0,
        skip: 0,
        discard: 0,
        phase_min: i128::MIN,
        phase_max: i128::MAX,
        payload_sha256: String::new(),
    };
    let mut last_pos = None;
    let mut hash = Sha256::new();
    for (index, packet) in packets.iter().enumerate() {
        let pos = number(packet, "pos")?;
        if pos < 0 || packet["stream_index"].as_u64() != Some(u64::from(source.audio_stream_index))
        {
            return Err("missing original packet position or changed audio selection".into());
        }
        if last_pos != Some(pos) {
            if last_pos.is_some_and(|last| pos <= last) {
                return Err("block positions not ordered".into());
            }
            let phase =
                i128::from(number(packet, "pts")?) * rate_scale - index as i128 * 1024 * 1000;
            // One original timestamp tick bounds unknown floor/nearest rounding.
            // Every independent block constrains this SAME phase, never resets it.
            proof.phase_min = proof.phase_min.max(phase - rate_scale);
            proof.phase_max = proof.phase_max.min(phase + rate_scale);
            proof.anchors += 1;
            last_pos = Some(pos);
        }
        let (skip, discard) = padding(packet)?;
        if (skip > 0 && index != 0)
            || (discard > 0 && index + 1 != packets.len())
            || skip > 1024
            || discard >= 1024
        {
            return Err("unsupported interior padding".into());
        }
        proof.skip += skip;
        proof.discard += discard;
        let payload = hex(packet["data"].as_str().ok_or("missing encoded payload")?)?;
        if payload.len() != number(packet, "size")? as usize
            || payload.is_empty()
            || payload.len() + 7 >= 8192
        {
            return Err("invalid AAC access-unit size".into());
        }
        hash.update((payload.len() as u64).to_le_bytes());
        hash.update(&payload);
        let n = payload.len() + 7;
        proof.adts.extend_from_slice(&[
            0xff,
            0xf1,
            if rate == 48_000 { 0x4c } else { 0x50 },
            0x80 | (n >> 11) as u8,
            (n >> 3) as u8,
            ((n & 7) << 5) as u8 | 31,
            0xfc,
        ]);
        proof.adts.extend_from_slice(&payload);
    }
    if proof.anchors < 2 || proof.phase_min > proof.phase_max {
        return Err("source_block_clock_discontinuity".into());
    }
    proof.payload_sha256 = format!("{:x}", hash.finalize());
    Ok(proof)
}

fn validate_stream(
    json: &Value,
    stream: &Value,
    source: &CaptureSourceFacts,
) -> Result<u32, String> {
    let rate = source
        .native_sample_rate
        .filter(|rate| matches!(rate, 44_100 | 48_000))
        .ok_or("unsupported native sample rate")?;
    if !crate::capture_profile::quantized_aac(source)
        || stream["index"].as_u64() != Some(u64::from(source.audio_stream_index))
        || stream["codec_name"] != "aac"
        || stream["profile"] != "LC"
        || stream["sample_rate"]
            .as_str()
            .and_then(|value| value.parse::<u32>().ok())
            != Some(rate)
        || stream["channels"] != 2
        || stream["time_base"] != "1/1000"
        || !json["format"]["format_name"]
            .as_str()
            .is_some_and(|name| name.split(',').any(|name| name == "matroska"))
        || json["format"]["start_time"]
            .as_str()
            .and_then(super::super::source_facts::decimal_seconds_us)
            != source.format_start_time_us
    {
        return Err("unsupported AAC/Matroska clock or changed source facts".into());
    }
    let asc = hex(stream["extradata"]
        .as_str()
        .ok_or("missing AudioSpecificConfig")?)?;
    // LC, measured 44.1/48kHz, stereo, 1024 samples; trailing sync-extension bits are
    // accepted only for the measured FFmpeg LC config with SBR absent.
    let config = if rate == 48_000 {
        [0x11, 0x90]
    } else {
        [0x12, 0x10]
    };
    if asc != config && asc != [config[0], config[1], 0x56, 0xe5, 0x00] {
        return Err("unsupported AudioSpecificConfig".into());
    }
    Ok(rate)
}

fn validate_frames(
    packets: &[&Value],
    frames: &[(&Value, usize)],
    source: &CaptureSourceFacts,
    stream: &Value,
) -> Result<(), String> {
    let mut next = 0;
    for (frame, seen) in frames {
        if frame["stream_index"].as_u64() != Some(u64::from(source.audio_stream_index)) {
            return Err("decoded audio selection changed".into());
        }
        let matches = (next..*seen)
            .filter(|index| {
                number(packets[*index], "pos").ok() == number(frame, "pkt_pos").ok()
                    && number(packets[*index], "size").ok() == number(frame, "pkt_size").ok()
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err("ambiguous packet/frame association".into());
        }
        let index = matches[0];
        if index != next {
            let (skip, _) = padding(packets[0])?;
            if next != 0
                || index != 1
                || (skip != 1024 && stream["initial_padding"].as_u64() != Some(1024))
            {
                return Err("unobserved decoded access unit".into());
            }
        }
        let (skip, discard) = padding(packets[index])?;
        if frame["nb_samples"].as_u64() != 1024u64.checked_sub(skip + discard)
            || number(frame, "pts")? != number(packets[index], "pts")?
        {
            return Err("AAC native sample observation mismatch".into());
        }
        next = index + 1;
    }
    if next != packets.len() {
        return Err("missing terminal frame observation".into());
    }
    Ok(())
}

fn padding(packet: &Value) -> Result<(u64, u64), String> {
    let Some(items) = packet["side_data_list"].as_array() else {
        return Ok((0, 0));
    };
    let mut padding = None;
    for item in items {
        if item["side_data_type"] != "Skip Samples" || padding.is_some() {
            return Err("unsupported packet side data".into());
        }
        padding = Some((
            item["skip_samples"]
                .as_u64()
                .ok_or("missing skip samples")?,
            item["discard_padding"]
                .as_u64()
                .ok_or("missing discard samples")?,
        ));
    }
    Ok(padding.unwrap_or_default())
}

fn number(value: &Value, name: &str) -> Result<i64, String> {
    value[name]
        .as_i64()
        .or_else(|| value[name].as_str().and_then(|raw| raw.parse().ok()))
        .ok_or_else(|| format!("missing {name}"))
}

fn hex(raw: &str) -> Result<Vec<u8>, String> {
    let mut result = Vec::new();
    for line in raw.lines().filter(|line| !line.is_empty()) {
        let (_, fields) = line.split_once(": ").ok_or("malformed encoded hexdump")?;
        let (data, _) = fields
            .split_once("  ")
            .ok_or("malformed encoded hexdump suffix")?;
        let data = data.replace(' ', "");
        if data.len() % 2 != 0 {
            return Err("malformed encoded hex length".into());
        }
        for pair in data.as_bytes().as_chunks::<2>().0 {
            let pair = std::str::from_utf8(pair).map_err(|_| "invalid encoded hex")?;
            result.push(u8::from_str_radix(pair, 16).map_err(|_| "invalid encoded hex")?);
        }
    }
    Ok(result)
}
