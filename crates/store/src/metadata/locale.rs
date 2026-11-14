//! One locale selection rule shared by card reduction and detail reads.
//!
//! For every chain candidate in order: an exact match, then a language-prefix match
//! (`zh` ↔ `zh-CN`); ties resolve by the sorted stored locale. When no candidate
//! matches, [`LocaleFallback`] decides: adopted metadata may fall back to its first
//! stored locale; NFO inputs may only use the unbound (`''`) bucket, so a foreign
//! imported NFO never leaks into another language.

/// What to return when no chain candidate matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocaleFallback {
    /// Adopted metadata: the first stored locale (sorted).
    AdoptedFirst,
    /// Library inputs: only the unbound `''` bucket.
    UnboundInputOnly,
}

fn language(locale: &str) -> &str {
    locale.split(['-', '_']).next().unwrap_or(locale)
}

/// Pure selector; `available` need not be sorted.
pub fn select_locale(
    available: &[String],
    chain: &[String],
    fallback: LocaleFallback,
) -> Option<String> {
    let mut sorted: Vec<&String> = available.iter().collect();
    sorted.sort();
    sorted.dedup();
    for candidate in chain.iter().map(|c| c.trim()).filter(|c| !c.is_empty()) {
        if let Some(exact) = sorted.iter().find(|s| s.as_str() == candidate) {
            return Some((*exact).clone());
        }
        let wanted = language(candidate);
        if let Some(prefix) = sorted
            .iter()
            .find(|s| !s.is_empty() && language(s).eq_ignore_ascii_case(wanted))
        {
            return Some((*prefix).clone());
        }
    }
    match fallback {
        LocaleFallback::AdoptedFirst => sorted.first().map(|s| (*s).clone()),
        LocaleFallback::UnboundInputOnly => sorted.into_iter().find(|s| s.is_empty()).cloned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn exact_then_prefix_then_fallback() {
        let stored = v(&["en-US", "zh-CN"]);
        let pick = |chain: &[&str], f| select_locale(&stored, &v(chain), f);
        assert_eq!(
            pick(&["zh"], LocaleFallback::AdoptedFirst).as_deref(),
            Some("zh-CN")
        );
        assert_eq!(
            pick(&["en-US", "zh"], LocaleFallback::AdoptedFirst).as_deref(),
            Some("en-US")
        );
        assert_eq!(
            pick(&["ja"], LocaleFallback::AdoptedFirst).as_deref(),
            Some("en-US")
        );
        assert_eq!(pick(&["ja"], LocaleFallback::UnboundInputOnly), None);
        let with_unbound = v(&["", "ja-JP"]);
        assert_eq!(
            select_locale(
                &with_unbound,
                &v(&["zh-CN"]),
                LocaleFallback::UnboundInputOnly
            )
            .as_deref(),
            Some("")
        );
    }

    #[test]
    fn prefix_ties_resolve_by_sorted_locale() {
        let stored = v(&["zh-TW", "zh-CN"]);
        assert_eq!(
            select_locale(&stored, &v(&["zh"]), LocaleFallback::AdoptedFirst).as_deref(),
            Some("zh-CN")
        );
    }
}
