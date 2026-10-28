use domain::Media;

/// Localized search names can differ from the file's regional name. Consult
/// bounded candidate details for translations/aliases before giving up.
pub(super) fn find_catalog_match(
    catalog: &dyn crate::catalog::Catalog,
    wanted: &Media,
    query: &str,
    hits: &[media::CatalogHit],
) -> Option<media::CatalogHit> {
    let eligible: Vec<_> = hits
        .iter()
        .filter(|hit| hit.media.kind == wanted.kind && hit.media.tmdb_id.is_some())
        .filter(|hit| {
            wanted
                .year
                .is_none_or(|year| hit.media.year.is_none_or(|found| found == year))
        })
        .cloned()
        .collect();
    if let Some(hit) = find_best_catalog_match(query, wanted.year, &eligible) {
        return Some(hit.clone());
    }
    if eligible.len() > 5 {
        return None;
    }
    let enriched: Vec<_> = eligible
        .into_iter()
        .map(|mut hit| {
            let Some(id) = hit.media.tmdb_id.as_deref() else {
                return hit;
            };
            match catalog.details(wanted.kind, id) {
                Ok(Some(details))
                    if details.kind == wanted.kind
                        && details.tmdb_id == hit.media.tmdb_id
                        && wanted
                            .year
                            .is_none_or(|year| details.year.is_none_or(|found| found == year)) =>
                {
                    hit.media
                        .aliases
                        .extend(catalog_names(&details).map(str::to_string));
                    if hit.media.year.is_none() {
                        hit.media.year = details.year;
                    }
                }
                Err(error) => tracing::error!(%error, tmdb_id = %id, "扫描候选别名读取失败"),
                _ => {}
            }
            hit
        })
        .collect();
    find_best_catalog_match(query, wanted.year, &enriched).cloned()
}

fn find_best_catalog_match<'a>(
    title: &str,
    year: Option<u16>,
    hits: &'a [media::CatalogHit],
) -> Option<&'a media::CatalogHit> {
    let norm_target = normalize_title(title);
    if norm_target.is_empty() {
        return None;
    }
    let eligible: Vec<_> = hits
        .iter()
        .filter(|hit| hit.media.tmdb_id.is_some())
        .filter(|hit| year.is_none_or(|target| hit.media.year.is_none_or(|found| found == target)))
        .collect();
    let exact: Vec<_> = eligible
        .iter()
        .copied()
        .filter(|hit| catalog_names(&hit.media).any(|name| normalize_title(name) == norm_target))
        .collect();
    // Prefer the known year, but never pick the first of conflicting identities.
    if let Some(target_year) = year {
        let dated: Vec<_> = exact
            .iter()
            .copied()
            .filter(|hit| hit.media.year == Some(target_year))
            .collect();
        if !dated.is_empty() {
            return unique_catalog_match(&dated);
        }
    }
    if !exact.is_empty() {
        return unique_catalog_match(&exact);
    }
    // Partial names are only safe when the catalog itself has a single candidate.
    if eligible.len() == 1
        && catalog_names(&eligible[0].media).any(|name| {
            let name = normalize_title(name);
            !name.is_empty() && (name.contains(&norm_target) || norm_target.contains(&name))
        })
    {
        return Some(eligible[0]);
    }

    None
}

fn catalog_names(media: &Media) -> impl Iterator<Item = &str> {
    std::iter::once(media.title.as_str())
        .chain(media.original_title.as_deref())
        .chain(media.aliases.iter().map(String::as_str))
}

fn unique_catalog_match<'a>(hits: &[&'a media::CatalogHit]) -> Option<&'a media::CatalogHit> {
    let first = *hits.first()?;
    hits.iter()
        .all(|hit| hit.media.kind == first.media.kind && hit.media.tmdb_id == first.media.tmdb_id)
        .then_some(first)
}

/// 文件名标题优先；上层目录里和它不同的标题（常见是中文别名）作为后续搜索词。
pub(super) fn search_queries(title: &str, sample_path: &std::path::Path) -> Vec<String> {
    let mut queries = vec![title.to_string()];
    let mut current = sample_path.parent();
    let mut depth = 0;
    while let Some(dir) = current {
        if depth >= 4 {
            break;
        }
        depth += 1;
        if let Some(name) = dir.file_name().and_then(|name| name.to_str()) {
            let parsed = release::parse(name);
            let candidate = parsed.title.trim();
            if parsed.confidence != domain::Confidence::Low
                && !candidate.is_empty()
                && normalize_title(candidate) != normalize_title(title)
                && !queries
                    .iter()
                    .any(|query| normalize_title(query) == normalize_title(candidate))
            {
                queries.push(candidate.to_string());
            }
        }
        current = dir.parent();
    }
    queries
}

fn normalize_title(t: &str) -> String {
    t.chars()
        .filter(|c| {
            !c.is_whitespace() && *c != '.' && *c != '-' && *c != '_' && *c != ':' && *c != '：'
        })
        .collect::<String>()
        .to_lowercase()
}
