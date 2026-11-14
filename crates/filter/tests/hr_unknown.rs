use domain::{AtomRule, Filter, FilterAtom, FilterId, SiteId, Torrent, UnknownHrPolicy};

fn torrent(hr: Option<bool>) -> Torrent {
    Torrent {
        site_id: SiteId::new(),
        title: "Movie.2026.1080p.WEB-DL.x264".into(),
        enclosure: "https://pt.example/download/1".into(),
        size_bytes: None,
        seeders: None,
        free: false,
        hr,
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

fn hr_filter(unknown: UnknownHrPolicy) -> Filter {
    Filter::new(
        FilterId::new(),
        "HR",
        vec![FilterAtom {
            priority: 100,
            rule: AtomRule::Hr { unknown },
            exclude: true,
        }],
    )
}

#[test]
fn hr_atom_with_unknown_exclude_drops_unknown_hr_release() {
    let outcome = filter::admit(vec![torrent(None)], &hr_filter(UnknownHrPolicy::Exclude));
    assert!(outcome.admitted.is_empty(), "unknown HR must be excluded");
    assert_eq!(outcome.rejected.len(), 1);
}

#[test]
fn hr_unknown_policy_preserves_known_hr_and_known_non_hr_decisions() {
    for policy in [UnknownHrPolicy::Exclude, UnknownHrPolicy::Allow] {
        let outcome = filter::admit(
            vec![torrent(Some(true)), torrent(Some(false))],
            &hr_filter(policy),
        );
        assert_eq!(outcome.admitted.len(), 1);
        assert_eq!(outcome.admitted[0].torrent.hr, Some(false));
        assert_eq!(outcome.rejected.len(), 1);
        assert_eq!(outcome.rejected[0].torrent.hr, Some(true));
    }
}

#[test]
fn hr_atom_with_unknown_allow_admits_unknown_hr_release() {
    let outcome = filter::admit(vec![torrent(None)], &hr_filter(UnknownHrPolicy::Allow));
    assert_eq!(outcome.admitted.len(), 1);
    assert!(outcome.rejected.is_empty());
}

#[test]
fn platform_atoms_match_canonical_markers_and_team_atoms_match_parsed_group() {
    for (rule, title, expected) in [
        (
            AtomRule::Platform("netflix".into()),
            "Movie.2026.NF.1080p.WEB-DL-GROUP",
            true,
        ),
        (
            AtomRule::Platform("NF".into()),
            "Movie.NFabulous.2026.1080p.WEB-DL-GROUP",
            false,
        ),
        (
            AtomRule::Team("GROUP".into()),
            "Movie.2026.NF.1080p.WEB-DL-group",
            true,
        ),
        (
            AtomRule::Team("GROUP".into()),
            "GROUP.Movie.2026.1080p.WEB-DL-OTHER",
            false,
        ),
    ] {
        let mut candidate = torrent(Some(false));
        candidate.title = title.into();
        let filter = Filter::new(
            FilterId::new(),
            "Platform/team",
            vec![FilterAtom {
                priority: 100,
                rule,
                exclude: false,
            }],
        );
        let outcome = filter::admit(vec![candidate], &filter);
        assert_eq!(!outcome.admitted.is_empty(), expected, "{title}");
        assert_eq!(outcome.rejected.len(), usize::from(!expected), "{title}");
    }
}
