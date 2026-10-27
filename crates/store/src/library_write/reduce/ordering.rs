//! Ordering keys for `library_items`: resolution rank and the stable shuffle key.

/// Rank order used by the `resolution` sort/filter and by `library_items.resolution_rank`.
pub(crate) fn resolution_rank(value: &str) -> i64 {
    match value {
        "2160p" | "4k" => 4,
        "1080p" | "1080i" => 3,
        "720p" => 2,
        _ => 1,
    }
}

/// Stable pseudo-random order key: FNV-1a over the media id, zero-padded for text order.
pub(crate) fn shuffle_key(media_id: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in media_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
