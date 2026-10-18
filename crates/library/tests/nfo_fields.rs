use domain::{EntityScope, FieldValue, MetadataCast, MetadataFields};
use library::nfo::{parse_nfo, parse_nfo_fields, NfoInput, NfoReadOutcome};

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
