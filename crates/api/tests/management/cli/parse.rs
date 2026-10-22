use api::cli::Command;

#[test]
fn empty_argv_is_serve() {
    assert_eq!(Command::parse(&[] as &[&str]).unwrap(), Command::Serve);
}

#[test]
fn sites_add_without_name_is_error() {
    assert!(
        Command::parse(&[
            "sites",
            "--add",
            "--url",
            "https://pt.example/",
            "--profile",
            "demo"
        ])
        .is_err()
    );
}

#[test]
fn sites_disable_without_id_is_error() {
    assert!(Command::parse(&["sites", "--disable"]).is_err());
}

#[test]
fn sites_enable_without_id_is_error() {
    assert!(Command::parse(&["sites", "--enable"]).is_err());
}

#[test]
fn library_extra_arg_is_error() {
    assert!(Command::parse(&["library", "extra"]).is_err());
}

#[test]
fn library_parses() {
    assert_eq!(Command::parse(&["library"]).unwrap(), Command::Library);
}

#[test]
fn ledger_parses() {
    assert_eq!(Command::parse(&["ledger"]).unwrap(), Command::Ledger);
}

#[test]
fn ledger_extra_arg_is_error() {
    assert!(Command::parse(&["ledger", "extra"]).is_err());
}

#[test]
fn directory_extra_arg_is_error() {
    assert!(Command::parse(&["directory", "extra"]).is_err());
}

#[test]
fn directory_parses() {
    assert_eq!(Command::parse(&["directory"]).unwrap(), Command::Directory);
}

#[test]
fn directory_add_root_without_path_is_error() {
    assert!(Command::parse(&["directory", "--add-root", "--kind", "movie"]).is_err());
}

#[test]
fn directory_remove_root_without_id_is_error() {
    assert!(Command::parse(&["directory", "--remove-root"]).is_err());
}

#[test]
fn directory_watch_inplace_without_path_is_error() {
    assert!(Command::parse(&["directory", "--watch-inplace"]).is_err());
}

#[test]
fn directory_watch_intake_without_path_is_error() {
    assert!(Command::parse(&["directory", "--watch-intake"]).is_err());
}

#[test]
fn directory_transfer_mode_without_value_is_error() {
    assert!(Command::parse(&["directory", "--transfer-mode"]).is_err());
}

#[test]
fn directory_transfer_mode_bogus_is_error() {
    assert!(Command::parse(&["directory", "--transfer-mode", "bogus"]).is_err());
}

#[test]
fn directory_scrape_without_value_is_error() {
    assert!(Command::parse(&["directory", "--scrape"]).is_err());
}

#[test]
fn directory_scrape_bogus_is_error() {
    assert!(Command::parse(&["directory", "--scrape", "maybe"]).is_err());
}

#[test]
fn directory_movie_naming_without_value_is_error() {
    assert!(Command::parse(&["directory", "--movie-naming"]).is_err());
}

#[test]
fn directory_tv_naming_without_value_is_error() {
    assert!(Command::parse(&["directory", "--tv-naming"]).is_err());
}

#[test]
fn downloads_extra_arg_is_error() {
    assert!(Command::parse(&["downloads", "extra"]).is_err());
}

#[test]
fn downloads_parses() {
    assert_eq!(Command::parse(&["downloads"]).unwrap(), Command::Downloads);
}

#[test]
fn subscribe_parses_as_list() {
    assert_eq!(Command::parse(&["subscribe"]).unwrap(), Command::Subscribes);
}

#[test]
fn filters_parses() {
    assert_eq!(Command::parse(&["filters"]).unwrap(), Command::Filters);
}

#[test]
fn filters_extra_arg_is_error() {
    assert!(Command::parse(&["filters", "extra"]).is_err());
}

#[test]
fn filters_add_without_name_is_error() {
    assert!(
        Command::parse(&[
            "filters",
            "--add",
            "--atom",
            "resolution=2160p",
            "--priority",
            "100"
        ])
        .is_err()
    );
}

#[test]
fn filters_add_without_atom_is_error() {
    assert!(Command::parse(&["filters", "--add", "--name", "uhd"]).is_err());
}

#[test]
fn filters_default_without_id_is_error() {
    assert!(Command::parse(&["filters", "--default"]).is_err());
}

#[test]
fn downloaders_parses() {
    assert_eq!(
        Command::parse(&["downloaders"]).unwrap(),
        Command::Downloaders
    );
}

#[test]
fn downloaders_extra_arg_is_error() {
    assert!(Command::parse(&["downloaders", "extra"]).is_err());
}

#[test]
fn downloaders_add_without_name_is_error() {
    assert!(
        Command::parse(&[
            "downloaders",
            "--add",
            "--kind",
            "qbittorrent",
            "--url",
            "http://qb-a:8080"
        ])
        .is_err()
    );
}

#[test]
fn downloaders_default_without_id_is_error() {
    assert!(Command::parse(&["downloaders", "--default"]).is_err());
}

#[test]
fn users_parses() {
    assert_eq!(Command::parse(&["users"]).unwrap(), Command::Users);
}

#[test]
fn users_extra_arg_is_error() {
    assert!(Command::parse(&["users", "extra"]).is_err());
}

#[test]
fn users_add_without_login_is_error() {
    assert!(Command::parse(&["users", "--add", "--token", "alice-token"]).is_err());
}
#[test]
fn unidentified_extra_arg_is_error() {
    assert!(Command::parse(&["unidentified", "extra"]).is_err());
}

#[test]
fn unidentified_parses() {
    assert_eq!(
        Command::parse(&["unidentified"]).unwrap(),
        Command::Unidentified
    );
}

#[test]
fn unidentified_claim_without_path_is_error() {
    assert!(
        Command::parse(&[
            "unidentified",
            "--claim",
            "--title",
            "The Matrix",
            "--kind",
            "movie"
        ])
        .is_err()
    );
}

#[test]
fn jobs_parses_as_list() {
    assert_eq!(Command::parse(&["jobs"]).unwrap(), Command::Jobs);
}

#[test]
fn jobs_extra_arg_is_error() {
    assert!(Command::parse(&["jobs", "extra"]).is_err());
}

#[test]
fn jobs_tick_now_parses() {
    assert_eq!(
        Command::parse(&["jobs", "tick", "--now", "1"]).unwrap(),
        Command::JobsTick { now: Some(1) }
    );
}

#[test]
fn search_without_query_is_error() {
    assert!(Command::parse(&["search"]).is_err());
}

#[test]
fn catalog_without_query_is_error() {
    assert!(Command::parse(&["catalog", "--query"]).is_err());
}

#[test]
fn catalog_parses_as_list() {
    assert_eq!(Command::parse(&["catalog"]).unwrap(), Command::Catalog);
}

#[test]
fn catalog_delete_without_key_is_error() {
    assert!(Command::parse(&["catalog", "--delete", "--source", "tmdb"]).is_err());
}

#[test]
fn search_admit_without_subscribe_is_error() {
    assert!(
        Command::parse(&[
            "search",
            "--admit",
            "--enclosure",
            "https://pt.example/download.php?id=1"
        ])
        .is_err()
    );
}

