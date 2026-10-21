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
    assert_eq!(episode.fields.air_date, FieldValue::Value("2019-03-01".into()));
    assert_eq!(episode.fields.release_date, FieldValue::Missing);

    let outcome = parse_nfo_fields(
        "<episodedetails><aired>2019-03-02</aired><premiered>2019-03-01</premiered></episodedetails>",
        episode_scope,
    );
    let NfoReadOutcome::Valid(episode) = outcome else {
        panic!("Nfo must parse as valid");
    };
    assert_eq!(episode.fields.air_date, FieldValue::Value("2019-03-02".into()));
    assert_eq!(episode.fields.release_date, FieldValue::Missing);
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
