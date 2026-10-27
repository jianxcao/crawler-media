use domain::{EntityScope, FieldValue};
use library::nfo::{NfoReadOutcome, parse_nfo_fields};

#[test]
fn nfo_reader_extracts_typed_metadata_fields_and_origins() {
    let xml = r#"<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<movie>
    <title>Interstellar</title>
    <originaltitle>Interstellar</originaltitle>
    <year>2014</year>
    <plot>A team of explorers travel through a wormhole in space.</plot>
    <tagline>Mankind was born on Earth. It was never meant to die here.</tagline>
    <rating>8.7</rating>
    <votes>1500000</votes>
    <runtime>169</runtime>
    <mpaa>PG-13</mpaa>
    <genre>Adventure</genre>
    <genre>Drama</genre>
    <genre>Science Fiction</genre>
    <actor>
        <name>Matthew McConaughey</name>
        <role>Cooper</role>
        <order>0</order>
    </actor>
</movie>"#;

    let outcome = parse_nfo_fields(xml, EntityScope::Work);
    let NfoReadOutcome::Valid(nfo) = outcome else {
        panic!("Nfo must parse as valid");
    };

    assert_eq!(nfo.fields.title, FieldValue::Value("Interstellar".into()));
    assert_eq!(nfo.fields.year, FieldValue::Value(2014));
    assert_eq!(nfo.fields.rating, FieldValue::Value(8.7));
    assert_eq!(nfo.fields.runtime_minutes, FieldValue::Value(169));
    assert_eq!(nfo.fields.content_rating, FieldValue::Value("PG-13".into()));

    let genres = nfo.fields.genres.value().unwrap();
    assert_eq!(genres.len(), 3);
    assert_eq!(genres[0].name, "Adventure");
    // Action/Adventure genre mapping
    assert_eq!(genres[0].filter_key, "12"); // TMDB id for Adventure is 12

    let cast = nfo.fields.cast.value().unwrap();
    assert_eq!(cast.len(), 1);
    assert_eq!(cast[0].name, "Matthew McConaughey");
    assert_eq!(cast[0].role.as_deref(), Some("Cooper"));
}

#[test]
fn element_presence_decides_missing_versus_empty() {
    let outcome = parse_nfo_fields(
        "<movie><title>Interstellar</title><plot/></movie>",
        EntityScope::Work,
    );
    let NfoReadOutcome::Valid(nfo) = outcome else {
        panic!("Nfo must parse as valid");
    };
    assert_eq!(nfo.fields.overview, FieldValue::Empty);
    assert_eq!(nfo.fields.tagline, FieldValue::Missing);

    let outcome = parse_nfo_fields("<movie><plot>   </plot></movie>", EntityScope::Work);
    let NfoReadOutcome::Valid(blank) = outcome else {
        panic!("Nfo must parse as valid");
    };
    assert_eq!(blank.fields.overview, FieldValue::Empty);

    // An empty leading alias must not shadow a later alias that carries text.
    let outcome = parse_nfo_fields(
        "<movie><plot/><overview>Real plot</overview></movie>",
        EntityScope::Work,
    );
    let NfoReadOutcome::Valid(aliased) = outcome else {
        panic!("Nfo must parse as valid");
    };
    assert_eq!(
        aliased.fields.overview,
        FieldValue::Value("Real plot".into())
    );
}

#[test]
fn nfo_reader_maps_dates_studios_directors_and_creators() {
    let xml = r#"<tvshow>
        <title>Show</title>
        <premiered>2019-03-01</premiered>
        <enddate>2022-05-10</enddate>
        <studio>Studio X</studio>
        <studio>Studio Y</studio>
        <director>Director D</director>
        <creator>Creator C</creator>
    </tvshow>"#;
    let outcome = parse_nfo_fields(xml, EntityScope::Work);
    let NfoReadOutcome::Valid(nfo) = outcome else {
        panic!("Nfo must parse as valid");
    };
    assert_eq!(
        nfo.fields.release_date,
        FieldValue::Value("2019-03-01".into())
    );
    assert_eq!(nfo.fields.end_date, FieldValue::Value("2022-05-10".into()));
    assert_eq!(
        nfo.fields.studios,
        FieldValue::Value(vec!["Studio X".into(), "Studio Y".into()])
    );
    assert_eq!(
        nfo.fields.directors,
        FieldValue::Value(vec!["Director D".into()])
    );
    assert_eq!(
        nfo.fields.creators,
        FieldValue::Value(vec!["Creator C".into()])
    );

    // 这些字段同样遵守“元素存在性决定存在”：`<aired/>` 是 Empty，没写的是 Missing。
    let outcome = parse_nfo_fields(
        "<episodedetails><aired/></episodedetails>",
        EntityScope::Episode {
            season: 1,
            episode: 1,
        },
    );
    let NfoReadOutcome::Valid(episode) = outcome else {
        panic!("Nfo must parse as valid");
    };
    assert_eq!(episode.fields.air_date, FieldValue::Empty);
    assert_eq!(episode.fields.release_date, FieldValue::Missing);
    assert_eq!(episode.fields.studios, FieldValue::Missing);

    // episode 的 `<premiered>` 与 `<aired>` 同义（都是首播日期）→ air_date，不重复写 release_date；
    // 两者同时存在时以 episode NFO 的正式字段 `<aired>` 为准。
    let episode_scope = EntityScope::Episode {
        season: 1,
        episode: 2,
    };
    let outcome = parse_nfo_fields(
        "<episodedetails><premiered>2019-03-01</premiered></episodedetails>",
        episode_scope,
    );
    let NfoReadOutcome::Valid(episode) = outcome else {
        panic!("Nfo must parse as valid");
    };
    assert_eq!(
        episode.fields.air_date,
        FieldValue::Value("2019-03-01".into())
    );
    assert_eq!(episode.fields.release_date, FieldValue::Missing);

    let outcome = parse_nfo_fields(
        "<episodedetails><aired>2019-03-02</aired><premiered>2019-03-01</premiered></episodedetails>",
        episode_scope,
    );
    let NfoReadOutcome::Valid(episode) = outcome else {
        panic!("Nfo must parse as valid");
    };
    assert_eq!(
        episode.fields.air_date,
        FieldValue::Value("2019-03-02".into())
    );
    assert_eq!(episode.fields.release_date, FieldValue::Missing);
}

/// 页内主图只认已采纳事实：观察层必须把 episode 的 `<thumb>` 映射进 `still_url`，
/// 否则「只有 NFO `<thumb>` 有图」的剧集在页内会静默丢主图（端点却仍能返回图片）。
/// `still_url` 只落在 `media_episodes`，所以 Work/Season 作用域不得盲映射。
#[test]
fn episode_thumb_maps_to_still_url_but_work_and_season_do_not() {
    let episode_scope = EntityScope::Episode {
        season: 1,
        episode: 1,
    };
    let outcome = parse_nfo_fields(
        "<episodedetails><title>Pilot</title>\
         <thumb>https://image.tmdb.org/t/p/w500/x.jpg</thumb></episodedetails>",
        episode_scope,
    );
    let NfoReadOutcome::Valid(episode) = outcome else {
        panic!("Nfo must parse as valid");
    };
    assert_eq!(
        episode.fields.still_url,
        FieldValue::Value("https://image.tmdb.org/t/p/w500/x.jpg".into()),
        "episode 的 NFO <thumb> 必须成为观测到的 still_url"
    );

    // 存在性语义与其它字段一致：`<thumb/>` 是 Empty（显式清空），没写才是 Missing。
    let outcome = parse_nfo_fields("<episodedetails><thumb/></episodedetails>", episode_scope);
    let NfoReadOutcome::Valid(empty) = outcome else {
        panic!("Nfo must parse as valid");
    };
    assert_eq!(empty.fields.still_url, FieldValue::Empty);
    let outcome = parse_nfo_fields(
        "<episodedetails><title>Pilot</title></episodedetails>",
        episode_scope,
    );
    let NfoReadOutcome::Valid(missing) = outcome else {
        panic!("Nfo must parse as valid");
    };
    assert_eq!(missing.fields.still_url, FieldValue::Missing);

    let outcome = parse_nfo_fields(
        "<movie><thumb>https://image.tmdb.org/t/p/w500/m.jpg</thumb></movie>",
        EntityScope::Work,
    );
    let NfoReadOutcome::Valid(work) = outcome else {
        panic!("Nfo must parse as valid");
    };
    assert_eq!(
        work.fields.still_url,
        FieldValue::Missing,
        "Work 作用域没有 media_episodes.still_url，不得把 <thumb> 写进 still_url"
    );

    let outcome = parse_nfo_fields(
        "<season><thumb>https://image.tmdb.org/t/p/w500/s.jpg</thumb></season>",
        EntityScope::Season(1),
    );
    let NfoReadOutcome::Valid(season) = outcome else {
        panic!("Nfo must parse as valid");
    };
    assert_eq!(season.fields.still_url, FieldValue::Missing);
}

/// 页内主图事实只认可渲染的远端 URL。`<thumb>` 写成本地引用（相对路径、绝对路径）时属于
/// 文件系统旁车口径：把它落成 `still_url` 会盖掉已采纳的远端剧照，而页内主图渲染器
/// （`crates/api` 的 `remote_episode_thumb`）只认远端 URL，结果页内主图反而静默消失。
#[test]
fn local_file_thumb_is_not_a_page_still_url() {
    let episode_scope = EntityScope::Episode {
        season: 1,
        episode: 1,
    };
    for thumb in [
        "reference-still.jpg",
        "Show/Season 01/reference-still.jpg",
        "/data/library/episode-still.jpg",
    ] {
        let body = format!("<episodedetails><thumb>{thumb}</thumb></episodedetails>");
        let NfoReadOutcome::Valid(fields) = parse_nfo_fields(&body, episode_scope) else {
            panic!("Nfo must parse as valid");
        };
        assert_eq!(
            fields.fields.still_url,
            FieldValue::Missing,
            "本地引用 {thumb} 不是页内主图事实，不得写进 still_url"
        );
    }

    let NfoReadOutcome::Valid(remote) = parse_nfo_fields(
        "<episodedetails><thumb>https://image.example/still.jpg</thumb></episodedetails>",
        episode_scope,
    ) else {
        panic!("Nfo must parse as valid");
    };
    assert_eq!(
        remote.fields.still_url,
        FieldValue::Value("https://image.example/still.jpg".into()),
        "远端 URL 形态的 <thumb> 仍必须成为观测到的 still_url"
    );
}

#[test]
fn nfo_reader_outcome_types() {
    assert!(matches!(
        parse_nfo_fields("not valid xml", EntityScope::Work),
        NfoReadOutcome::Invalid { .. }
    ));

    assert!(matches!(
        parse_nfo_fields("<unsupported_tag></unsupported_tag>", EntityScope::Work),
        NfoReadOutcome::Unknown { .. }
    ));
}
