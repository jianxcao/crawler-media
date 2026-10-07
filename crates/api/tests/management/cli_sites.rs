use api::cli::{Command, Transport, run};

use super::cli::transport;

#[tokio::test]
async fn cli_sites_add_prints_name_and_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(
        Command::SitesAdd {
            name: "demo".into(),
            url: "https://pt.example/".into(),
            profile_id: "demo".into(),
            cookie: Some("uid=1".into()),
            api_key: None,
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("demo"), "{out}");
    let listed = run(Command::Sites, &t).await.unwrap();
    assert!(listed.contains("demo"), "{listed}");
}

#[tokio::test]
async fn cli_sites_disable_prints_enabled_false() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    run(
        Command::SitesAdd {
            name: "demo".into(),
            url: "https://pt.example/".into(),
            profile_id: "demo".into(),
            cookie: Some("uid=1".into()),
            api_key: None,
        },
        &t,
    )
    .await
    .unwrap();
    let listed = t.send("GET", "/api/v1/sites", None).await.unwrap();
    let id = listed.1[0]["id"].as_str().unwrap().to_string();
    let out = run(Command::SitesDisable { id }, &t).await.unwrap();
    assert!(out.contains("enabled=false"), "{out}");
}

#[tokio::test]
async fn cli_sites_enable_prints_enabled_true() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    run(
        Command::SitesAdd {
            name: "demo".into(),
            url: "https://pt.example/".into(),
            profile_id: "demo".into(),
            cookie: Some("uid=1".into()),
            api_key: None,
        },
        &t,
    )
    .await
    .unwrap();
    let listed = t.send("GET", "/api/v1/sites", None).await.unwrap();
    let id = listed.1[0]["id"].as_str().unwrap().to_string();
    run(Command::SitesDisable { id: id.clone() }, &t)
        .await
        .unwrap();
    let out = run(Command::SitesEnable { id }, &t).await.unwrap();
    assert!(out.contains("enabled=true"), "{out}");
}

#[tokio::test]
async fn cli_sites_list_prints_enabled_false_after_disable() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    run(
        Command::SitesAdd {
            name: "demo".into(),
            url: "https://pt.example/".into(),
            profile_id: "demo".into(),
            cookie: Some("uid=1".into()),
            api_key: None,
        },
        &t,
    )
    .await
    .unwrap();
    let listed = t.send("GET", "/api/v1/sites", None).await.unwrap();
    let id = listed.1[0]["id"].as_str().unwrap().to_string();
    run(Command::SitesDisable { id }, &t).await.unwrap();
    let out = run(Command::Sites, &t).await.unwrap();
    assert!(out.contains("enabled=false"), "{out}");
}
