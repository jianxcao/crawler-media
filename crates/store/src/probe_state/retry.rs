pub fn retry_delay_ms(failure_count: u32) -> Option<i64> {
    match failure_count {
        1 => Some(60_000),
        2 => Some(300_000),
        3 => Some(900_000),
        4 => Some(3_600_000),
        _ => None,
    }
}
