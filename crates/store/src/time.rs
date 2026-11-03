//! Store wall-clock and persisted timestamp formatting.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// RFC3339 UTC string for the current wall-clock time (second precision).
pub fn now_rfc3339() -> String {
    rfc3339_from_secs(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    )
}

/// Format a Unix timestamp as RFC3339 UTC (`YYYY-MM-DDTHH:MM:SSZ`), hand-rolled
/// to avoid a chrono dependency (civil-from-days, valid for our range).
pub fn rfc3339_from_secs(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let h = rem / 3600;
    let m = (rem % 3600) / 60;
    let sec = rem % 60;
    let month_days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut year = 1970i64;
    let mut d = days;
    loop {
        let ydays = if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
            366
        } else {
            365
        };
        if d < ydays {
            break;
        }
        d -= ydays;
        year += 1;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let mut month = 0usize;
    let mut day = d;
    for (i, mdays) in month_days.iter().enumerate() {
        let dim = if i == 1 && leap { 29 } else { *mdays };
        if day < dim {
            month = i + 1;
            break;
        }
        day -= dim;
    }
    format!(
        "{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{sec:02}Z",
        day = day + 1
    )
}

/// Checked wall-clock timestamp in Unix milliseconds.
pub(crate) fn unix_now_ms() -> Result<i64, crate::StoreError> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| {
            crate::StoreError::Invalid(format!("wall clock before Unix epoch: {error}"))
        })?;
    i64::try_from(elapsed.as_millis()).map_err(|error| {
        crate::StoreError::Invalid(format!("wall clock outside millisecond range: {error}"))
    })
}
