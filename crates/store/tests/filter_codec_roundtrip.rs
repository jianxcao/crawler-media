use domain::{AtomRule, Filter, FilterAtom, FilterId};
use store::Store;

#[test]
fn codec_filter_survives_database_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let id = FilterId::new();
    let filter = Filter {
        id,
        name: "codec".into(),
        atoms: vec![FilterAtom {
            priority: 90,
            rule: AtomRule::Codec("hevc".into()),
            exclude: false,
        }],
        keep_old_versions: true,
    };
    let store = Store::open(tmp.path()).unwrap();
    store.insert_filter(&filter).unwrap();
    drop(store);
    let reopened = Store::open(tmp.path()).unwrap();
    let saved = reopened.get_filter(id).unwrap().unwrap();
    assert_eq!(saved.atoms, filter.atoms);
    assert_eq!(saved.keep_old_versions, true);
}
