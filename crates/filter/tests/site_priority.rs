use domain::{AtomRule, Filter, FilterAtom, FilterId, SiteId, Torrent};

fn torrent(site_id: SiteId, title: &str) -> Torrent {
    Torrent {
        site_id,
        title: title.into(),
        enclosure: format!("https://example.test/{}", title.len()),
        size_bytes: Some(1_000_000_000),
        seeders: Some(100),
        free: false,
        hr: false,
        imdb_id: None,
        id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    }
}

fn filter(atoms: Vec<FilterAtom>) -> Filter {
    Filter {
        id: FilterId::new(),
        name: "site priority".into(),
        atoms,
        keep_old_versions: false,
    }
}

#[test]
fn preferred_site_beats_quality_score_on_other_site() {
    let preferred = SiteId::new();
    let other = SiteId::new();
    let f = filter(vec![
        FilterAtom {
            priority: 50,
            rule: AtomRule::Site(preferred.to_string()),
            exclude: false,
        },
        FilterAtom {
            priority: 900,
            rule: AtomRule::TitleMatch("2160p".into()),
            exclude: false,
        },
        FilterAtom {
            priority: 10,
            rule: AtomRule::TitleMatch("1080p".into()),
            exclude: false,
        },
    ]);
    let result = filter::admit_scored(
        vec![
            (torrent(preferred, "Movie.1080p.WEB-DL"), Some(release::parse("Movie.1080p.WEB-DL"))),
            (torrent(other, "Movie.2160p.Remux"), Some(release::parse("Movie.2160p.Remux"))),
        ],
        &f,
    );

    assert_eq!(result.admitted.len(), 2);
    assert!(result.admitted[0].score > result.admitted[1].score);
    assert_eq!(result.admitted[0].torrent.site_id, preferred);
}

#[test]
fn unmatched_preferred_site_falls_back_to_quality_match_elsewhere() {
    let preferred = SiteId::new();
    let fallback = SiteId::new();
    let f = filter(vec![
        FilterAtom {
            priority: 50,
            rule: AtomRule::Site(preferred.to_string()),
            exclude: false,
        },
        FilterAtom {
            priority: 100,
            rule: AtomRule::TitleMatch("1080p".into()),
            exclude: false,
        },
    ]);
    let result = filter::admit(vec![torrent(fallback, "Movie.1080p.WEB-DL")], &f);

    assert_eq!(result.admitted.len(), 1);
    assert_eq!(result.admitted[0].torrent.site_id, fallback);
    assert_eq!(result.admitted[0].score, 100);
}
