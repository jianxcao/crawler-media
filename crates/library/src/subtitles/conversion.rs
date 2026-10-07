/// 将 SRT 文本内容转换为规范的 WebVTT 格式。
/// 包含 WEBVTT 头部，并将时间戳中的逗号分隔符转换为点号毫秒（如 00:00:01,000 -> 00:00:01.000）。
pub fn srt_to_vtt(srt: &str) -> String {
    let mut vtt = String::from("WEBVTT\n\n");
    for line in srt.lines() {
        if line.contains("-->") {
            // 时间戳行：00:00:01,000 --> 00:00:02,000
            let converted = line.replace(',', ".");
            vtt.push_str(&converted);
            vtt.push('\n');
        } else {
            vtt.push_str(line);
            vtt.push('\n');
        }
    }
    vtt
}
