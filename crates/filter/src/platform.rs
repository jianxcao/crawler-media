/// Platform markers are separate title tokens, never arbitrary substrings.
pub(super) fn platform_matches(title: &str, want: &str) -> bool {
    let normalized = want.to_ascii_lowercase();
    let markers: &[&str] = match normalized.as_str() {
        "netflix" => &["netflix", "nf"],
        "amazon" => &["amazon", "amzn"],
        "disney_plus" => &["disney", "dsnp", "dsny"],
        "hbo_max" => &["hbo", "hmax", "max"],
        "apple_tv_plus" => &["atvp", "apple"],
        "paramount_plus" => &["pmtp", "paramount"],
        "discovery_plus" => &["discovery", "dscp"],
        "iqiyi" => &["iqiyi", "iq"],
        "wetv" => &["wetv", "tencent"],
        "mangotv" => &["mangotv", "mgtv"],
        "bilibili" => &["bilibili", "bili"],
        "crunchyroll" => &["crunchyroll", "cr"],
        _ => &[],
    };
    title.split(|c: char| !c.is_alphanumeric()).any(|token| {
        token.eq_ignore_ascii_case(&normalized)
            || markers
                .iter()
                .any(|marker| token.eq_ignore_ascii_case(marker))
    })
}
