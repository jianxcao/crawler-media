use domain::SourceFieldOrigin;

pub fn parse_locale(loc: &str) -> (&str, Option<&str>) {
    let mut parts = loc.split(['-', '_']);
    let lang = parts.next().unwrap_or(loc);
    let country = parts.next();
    (lang, country)
}
