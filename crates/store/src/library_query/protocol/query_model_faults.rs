use super::*;
use crate::test_support::{FixtureFile, publish_fixture_file};
use domain::{
    Confidence, Media, MediaId, MediaKind, ProtocolItemType, ProtocolParent, QualitySource, UserId,
};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

#[test]
fn classification_queries_do_not_read_cold_metadata_but_search_does() {
    for kind in [MediaKind::Movie, MediaKind::Tv] {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("data")).unwrap();
        let root = tmp.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        let lib = store
            .create_library(
                kind,
                "Movies",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media = Media {
            id: MediaId::new(),
            kind,
            title: "Film".into(),
            year: None,
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
            aliases: vec![],
        };
        store.insert_media(&media).unwrap();
        std::fs::write(root.join("Film.mkv"), b"file").unwrap();
        std::fs::write(root.join("Film.nfo"),"<movie><title>Film</title><genre>Drama</genre><studio>Studio</studio><actor><name>Jane</name></actor></movie>").unwrap();
        if kind == MediaKind::Tv {
            std::fs::write(root.join("tvshow.nfo"), "<tvshow><title>Film</title><genre>Drama</genre><studio>Studio</studio><actor><name>Jane</name></actor></tvshow>").unwrap();
            std::fs::write(root.join("Film.nfo"), "<episodedetails><season>1</season><episode>1</episode><title>Episode</title><genre>Episode genre</genre></episodedetails>").unwrap();
        }
        publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: media.id,
                season: (kind == MediaKind::Tv).then_some(1),
                episode: (kind == MediaKind::Tv).then_some(1),
                path: root.join("Film.mkv").display().to_string(),
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                ..Default::default()
            },
        )
        .unwrap();
        let mut request = ProtocolItemQuery {
            user_id: UserId::new(),
            parent: ProtocolParent::Library(lib.id.parse().unwrap()),
            series_id: None,
            season: None,
            person_ids: vec![],
            studio_ids: vec![],
            search_term: None,
            genres: vec![],
            genre_ids: vec![],
            filters: vec![],
            is_favorite: None,
            is_played: None,
            include_types: vec![if kind == MediaKind::Tv {
                ProtocolItemType::Episode
            } else {
                ProtocolItemType::Movie
            }],
            recursive: true,
            is_folder: None,
            sort: vec![],
            offset: 0,
            limit: 10,
        };
        store
            .library
            .authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
                AuthAction::Read {
                    table_name: "media_metadata" | "library_metadata_inputs" | "media_episodes",
                    ..
                } => Authorization::Deny,
                _ => Authorization::Allow,
            }));
        request.genres = vec!["Drama".into()];
        assert_eq!(store.protocol_item_page(&request).unwrap().total, 1);
        request.genres.clear();
        request.studio_ids = vec!["Studio".into()];
        assert_eq!(store.protocol_item_page(&request).unwrap().total, 1);
        request.studio_ids.clear();
        request.person_ids = vec![crate::protocol_index::person_wire_id("Actor", "Jane", None)];
        assert_eq!(store.protocol_item_page(&request).unwrap().total, 1);
        request.search_term = Some("Film".into());
        assert!(store.protocol_item_page(&request).is_err());
        store
            .library
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
    }
}
