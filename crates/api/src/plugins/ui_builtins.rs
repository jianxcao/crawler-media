use plugin_package::{UiManifest, UiSurfaceKind, UiSurfaceManifest};
use sha2::{Digest, Sha256};
use store::Store;
use store::plugins::GrantPluginUiTrust;

use super::reauth::now_unix;
use super::staging::hex_lower;
use super::types::format_rfc3339;
use super::ui_builtins_assets::{builtin_asset_manifest_bytes, lookup_builtin_ui_asset};

pub(crate) fn checkin_ui_manifest() -> UiManifest {
    UiManifest {
        kind: "federation-react".into(),
        contract: "1.0.0".into(),
        remote_name: "cmBuiltinCheckin".into(),
        entry: "ui/assets/remoteEntry.js".into(),
        asset_manifest: "ui/asset-manifest.json".into(),
        react: "19.3.0".into(),
        react_dom: "19.3.0".into(),
        capabilities: vec![
            "read:sites".into(),
            "read:history".into(),
            "command:site.maintenance".into(),
            "command:site.check-in".into(),
            "command:site.verify-session".into(),
            "task:run:check-in".into(),
        ],
        surfaces: vec![
            UiSurfaceManifest {
                id: "settings".into(),
                kind: UiSurfaceKind::Config,
                expose: "./Config".into(),
                title: "Check-in Settings".into(),
                icon: Some("sliders".into()),
            },
            UiSurfaceManifest {
                id: "main".into(),
                kind: UiSurfaceKind::Page,
                expose: "./Page".into(),
                title: "Site Check-in".into(),
                icon: Some("calendar-check".into()),
            },
            UiSurfaceManifest {
                id: "app".into(),
                kind: UiSurfaceKind::App,
                expose: "./App".into(),
                title: "Check-in Studio".into(),
                icon: Some("calendar-check".into()),
            },
            UiSurfaceManifest {
                id: "summary".into(),
                kind: UiSurfaceKind::Dashboard,
                expose: "./Dashboard".into(),
                title: "Check-in Status".into(),
                icon: Some("activity".into()),
            },
        ],
    }
}

pub(crate) fn boost_ui_manifest() -> UiManifest {
    UiManifest {
        kind: "federation-react".into(),
        contract: "1.0.0".into(),
        remote_name: "cmBuiltinBoost".into(),
        entry: "ui/assets/remoteEntry.js".into(),
        asset_manifest: "ui/asset-manifest.json".into(),
        react: "19.3.0".into(),
        react_dom: "19.3.0".into(),
        capabilities: vec![
            "read:sites".into(),
            "read:downloads".into(),
            "read:boost".into(),
            "command:downloader.delete-task".into(),
            "command:downloader.prepare-cleanup".into(),
            "command:downloader.delete-files".into(),
            "command:indexer.search".into(),
            "task:run:traffic-cycle".into(),
        ],
        surfaces: vec![
            UiSurfaceManifest {
                id: "settings".into(),
                kind: UiSurfaceKind::Config,
                expose: "./Config".into(),
                title: "Boost Settings".into(),
                icon: Some("sliders".into()),
            },
            UiSurfaceManifest {
                id: "main".into(),
                kind: UiSurfaceKind::Page,
                expose: "./Page".into(),
                title: "Boost Console".into(),
                icon: Some("gauge".into()),
            },
            UiSurfaceManifest {
                id: "app".into(),
                kind: UiSurfaceKind::App,
                expose: "./App".into(),
                title: "Boost Manager".into(),
                icon: Some("gauge".into()),
            },
            UiSurfaceManifest {
                id: "summary".into(),
                kind: UiSurfaceKind::Dashboard,
                expose: "./Dashboard".into(),
                title: "Boost Telemetry".into(),
                icon: Some("activity".into()),
            },
        ],
    }
}

pub(crate) fn builtin_bundle_sha256(plugin_id: &str) -> Option<String> {
    let manifest_bytes = builtin_asset_manifest_bytes(plugin_id)?;
    Some(hex_lower(&Sha256::digest(manifest_bytes)))
}

pub(crate) fn builtin_ui_asset(
    plugin_id: &str,
    asset_path: &str,
) -> Option<(&'static [u8], &'static str)> {
    lookup_builtin_ui_asset(plugin_id, asset_path)
}

pub(crate) fn ensure_builtin_ui_trust(store: &Store, plugin_id: &str, package_hash: &str) {
    let Some(bundle_sha256) = builtin_bundle_sha256(plugin_id) else {
        return;
    };
    if store
        .get_ui_trust(plugin_id, package_hash)
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }
    if let Err(err) = store.grant_ui_trust(&GrantPluginUiTrust {
        plugin_id: plugin_id.into(),
        package_sha256: package_hash.into(),
        bundle_sha256,
        granted_by_user_id: "system".into(),
        granted_at: format_rfc3339(now_unix()),
        expected_revision: 0,
    }) {
        tracing::warn!(%err, plugin_id, "failed to seed default builtin UI trust");
    }
}

#[cfg(test)]
mod tests {
    use plugin_package::UiAssetManifest;

    use super::super::boost::BUILTIN_BOOST_PLUGIN_ID;
    use super::super::builtins::BUILTIN_CHECKIN_PLUGIN_ID;
    use super::*;

    #[test]
    fn embedded_builtin_ui_bundles_match_manifests_and_exclude_duplicate_react() {
        for plugin_id in [BUILTIN_CHECKIN_PLUGIN_ID, BUILTIN_BOOST_PLUGIN_ID] {
            let manifest_bytes = builtin_asset_manifest_bytes(plugin_id).expect("manifest bytes");
            let expected_sha = hex_lower(&Sha256::digest(manifest_bytes));
            assert_eq!(
                builtin_bundle_sha256(plugin_id).as_deref(),
                Some(expected_sha.as_str())
            );

            let manifest: UiAssetManifest =
                serde_json::from_slice(manifest_bytes).expect("valid UiAssetManifest JSON");
            assert_eq!(manifest.version, 1);
            assert!(
                manifest
                    .files
                    .iter()
                    .any(|f| f.path == "ui/assets/remoteEntry.js"),
                "remoteEntry.js missing for {plugin_id}"
            );

            for file in &manifest.files {
                assert!(!file.path.ends_with(".html"));
                assert!(!file.path.ends_with(".map"));
                let (bytes, mime) =
                    builtin_ui_asset(plugin_id, &file.path).expect("embedded asset exists");
                assert!(!mime.is_empty());
                assert_eq!(bytes.len() as u64, file.size);
                assert_eq!(hex_lower(&Sha256::digest(bytes)), file.sha256);
                if file.path.ends_with(".js") {
                    let src = String::from_utf8_lossy(bytes);
                    assert!(
                        !src.contains("createInvalidHookError")
                            && !src.contains("react-dom.production"),
                        "duplicate react vendor runtime leaked into {plugin_id}/{}",
                        file.path
                    );
                }
            }

            let (entry_bytes, _) =
                builtin_ui_asset(plugin_id, "ui/assets/remoteEntry.js").expect("remoteEntry");
            let entry_src = String::from_utf8_lossy(entry_bytes);
            for expose in ["./Config", "./Page", "./App", "./Dashboard"] {
                assert!(
                    entry_src.contains(expose),
                    "{plugin_id} remoteEntry.js missing expose {expose}"
                );
            }
        }
    }
}
