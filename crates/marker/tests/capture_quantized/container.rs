use super::*;

pub(super) fn payloads(root: &Path) -> Vec<Vec<u8>> {
    payloads_seconds(root, 12)
}

pub(super) fn payloads_seconds(root: &Path, seconds: u32) -> Vec<Vec<u8>> {
    payloads_at_rate(root, seconds, 44_100)
}

pub(super) fn payloads_at_rate(root: &Path, seconds: u32, rate: u32) -> Vec<Vec<u8>> {
    let file = root.join("encoded.aac");
    ff(
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("anoisesrc=seed=42:sample_rate={rate}:duration={seconds}"),
            "-ac",
            "2",
            "-c:a",
            "aac",
            "-threads",
            "1",
            "-f",
            "adts",
        ],
        &file,
    );
    let bytes = std::fs::read(file).unwrap();
    let mut result = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        assert_eq!(&bytes[at..at + 2], &[0xff, 0xf1]);
        let length = ((usize::from(bytes[at + 3]) & 3) << 11)
            | (usize::from(bytes[at + 4]) << 3)
            | (usize::from(bytes[at + 5]) >> 5);
        result.push(bytes[at + 7..at + length].to_vec());
        at += length;
    }
    result
}
fn size(length: usize) -> Vec<u8> {
    for count in 1..=8 {
        if length < (1usize << (7 * count)) - 1 {
            return ((length as u64) | (1u64 << (7 * count))).to_be_bytes()[8 - count..].to_vec();
        }
    }
    panic!("fixture EBML size");
}
fn element(tag: &[u8], value: Vec<u8>) -> Vec<u8> {
    [tag, &size(value.len()), &value].concat()
}
fn uint(tag: &[u8], value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let first = bytes.iter().position(|b| *b != 0).unwrap_or(7);
    element(tag, bytes[first..].to_vec())
}
fn text(tag: &[u8], value: &str) -> Vec<u8> {
    element(tag, value.as_bytes().to_vec())
}

/// Actual Xiph lacing with alternating3/5/8 packet blocks; no periodicity guess.
pub(super) fn write(
    path: &Path,
    payloads: &[Vec<u8>],
    floor: bool,
    leading_ms: i64,
    shift: impl Fn(usize) -> i64,
) {
    write_at_rate(path, payloads, floor, leading_ms, shift, 44_100);
}

pub(super) fn write_at_rate(
    path: &Path,
    payloads: &[Vec<u8>],
    floor: bool,
    leading_ms: i64,
    shift: impl Fn(usize) -> i64,
    rate: u32,
) {
    let ebml = element(
        &[0x1a, 0x45, 0xdf, 0xa3],
        [
            uint(&[0x42, 0x86], 1),
            uint(&[0x42, 0xf7], 1),
            uint(&[0x42, 0xf2], 4),
            uint(&[0x42, 0xf3], 8),
            text(&[0x42, 0x82], "matroska"),
            uint(&[0x42, 0x87], 4),
            uint(&[0x42, 0x85], 2),
        ]
        .concat(),
    );
    let info = element(
        &[0x15, 0x49, 0xa9, 0x66],
        [
            uint(&[0x2a, 0xd7, 0xb1], 1_000_000),
            text(&[0x4d, 0x80], "capture proof fixture"),
            text(&[0x57, 0x41], "capture proof fixture"),
        ]
        .concat(),
    );
    let audio = element(
        &[0xe1],
        [
            element(&[0xb5], (rate as f64).to_be_bytes().to_vec()),
            uint(&[0x9f], 2),
        ]
        .concat(),
    );
    let track = element(
        &[0xae],
        [
            uint(&[0xd7], 1),
            uint(&[0x73, 0xc5], 1),
            uint(&[0x83], 2),
            uint(&[0x23, 0xe3, 0x83], 1_024_000_000_000 / u64::from(rate)),
            text(&[0x86], "A_AAC"),
            element(
                &[0x63, 0xa2],
                if rate == 48_000 {
                    vec![0x11, 0x90]
                } else {
                    vec![0x12, 0x10]
                },
            ),
            audio,
        ]
        .concat(),
    );
    let mut blocks = Vec::new();
    let mut at = 0;
    let mut group = 0;
    while at < payloads.len() {
        let count = [3, 5, 8][group % 3].min(payloads.len() - at);
        let numerator = at as i64 * 1024 * 1000;
        let pts = leading_ms
            + (numerator + if floor { 0 } else { i64::from(rate) / 2 }) / i64::from(rate)
            + shift(group);
        let mut block = vec![0x81];
        block.extend_from_slice(&(pts as i16).to_be_bytes());
        block.push(if count > 1 { 0x82 } else { 0x80 });
        if count > 1 {
            block.push(count as u8 - 1);
            for payload in &payloads[at..at + count - 1] {
                block.extend(std::iter::repeat_n(255, payload.len() / 255));
                block.push((payload.len() % 255) as u8);
            }
        }
        for payload in &payloads[at..at + count] {
            block.extend(payload);
        }
        blocks.push(element(&[0xa3], block));
        at += count;
        group += 1;
    }
    let cluster = element(
        &[0x1f, 0x43, 0xb6, 0x75],
        [uint(&[0xe7], 0), blocks.concat()].concat(),
    );
    let segment = element(
        &[0x18, 0x53, 0x80, 0x67],
        [info, element(&[0x16, 0x54, 0xae, 0x6b], track), cluster].concat(),
    );
    std::fs::write(path, [ebml, segment].concat()).unwrap();
}
