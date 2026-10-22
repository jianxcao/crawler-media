//! Admin-only route table (`auth::require_admin` is layered on by
//! `routes::admin_routes`).

use axum::Router;
use axum::routing::{delete, get, patch, post, put};

use crate::http::{
    directory, downloaders, file_delete, fs, job_maintenance, jobs, library, library_admin,
    library_artwork, library_chapters, library_config, library_delete, library_deletion,
    library_duplicates, library_housekeeping, library_operations, library_organize,
    library_presence, library_scan, media, media_operations, notify, playback, reidentify,
    rule_sets, scrape_settings, settings, sites, system_logs, users,
};
use crate::management::ApiState;

pub(super) fn admin_core_routes() -> Router<ApiState> {
    Router::new()
        // users
        .route("/users", post(users::create_user).get(users::list_users))
        .route(
            "/users/{id}",
            patch(users::update_user).delete(users::delete_user),
        )
        .route("/users/removals/{operation_id}", get(users::removal_status))
        // sites
        .route("/sites", post(sites::create_site))
        .route(
            "/sites/{id}",
            patch(sites::patch_site)
                .put(sites::put_site)
                .delete(sites::delete_site),
        )
        .route("/sites/{id}/verify", post(sites::verify_site))
        .route("/sites/{id}/login", post(sites::site_login))
        .route("/sites/{id}/check-in", post(sites::site_check_in))
        .route("/sites/{id}/boost", patch(sites::patch_boost))
        // rule-sets (Filter groups)
        .route("/rule-sets", post(rule_sets::create_rule_set))
        .route(
            "/rule-sets/{id}",
            patch(rule_sets::patch_rule_set).delete(rule_sets::delete_rule_set),
        )
        .route("/rule-sets/default", put(rule_sets::put_default_rule_set))
        // downloaders
        .route("/downloaders", post(downloaders::create_downloader))
        .route(
            "/downloaders/{id}",
            patch(downloaders::patch_downloader).delete(downloaders::delete_downloader),
        )
        .route(
            "/downloaders/{id}/verify",
            post(downloaders::verify_downloader),
        )
        .route(
            "/downloaders/target-prefs/{category}",
            put(downloaders::put_target_pref),
        )
        .route(
            "/downloaders/tasks/{hash}",
            delete(downloaders::delete_task),
        )
        .route(
            "/downloaders/tasks/{hash}/pause",
            post(downloaders::pause_task),
        )
        .route(
            "/downloaders/tasks/{hash}/resume",
            post(downloaders::resume_task),
        )
        .route("/downloaders/tasks/remove", post(downloaders::remove_task))
        .route(
            "/downloaders/tasks/replace",
            post(downloaders::replace_task),
        )
        .route(
            "/downloaders/limits",
            get(downloaders::get_limits).put(downloaders::set_limits),
        )
}

pub(super) fn admin_library_routes() -> Router<ApiState> {
    Router::new()
        // libraries：实体增删改 + 条目/文件删除
        .route(
            "/library-removals/{operation_id}",
            get(library_deletion::status),
        )
        .route(
            "/library/pending-deletions",
            get(file_delete::pending_deletions),
        )
        .route("/libraries", post(library_config::create_library))
        .route(
            "/libraries/{id}",
            patch(library_config::patch_library).delete(library_config::delete_library),
        )
        .route(
            "/libraries/{id}/default",
            put(library_config::set_default_library),
        )
        .route("/libraries/order", put(library_config::reorder_libraries))
        .route(
            "/libraries/routing-options",
            get(library_config::routing_options),
        )
        .route(
            "/libraries/{id}/items/{item_id}",
            delete(library_delete::delete_item),
        )
        .route(
            "/libraries/{id}/duplicates/{file_id}",
            delete(library_duplicates::delete_duplicate),
        )
        .route("/libraries/{id}/scan", post(library_scan::scan_library))
        .route(
            "/libraries/{id}/scan-operations/{operation_id}",
            get(library_scan::operations::get_scan_operation),
        )
        .route(
            "/libraries/{id}/scan-operations/{operation_id}/cancel",
            post(library_scan::operations::cancel_scan_operation),
        )
        // 根/语言重配置的进度查询：写请求 202 返回同一个 operation 形状。
        .route(
            "/libraries/{id}/operations/{operation_id}",
            get(library_operations::get_operation),
        )
        .route(
            "/libraries/{id}/integrity-checks",
            post(library_operations::integrity_checks),
        )
        .route(
            "/libraries/{id}/integrity-repairs",
            post(library_operations::integrity_repairs),
        )
        .route(
            "/libraries/{id}/verify",
            post(library_presence::verify_handler),
        )
        .route(
            "/libraries/{id}/missing-rows",
            delete(library_presence::prune_handler),
        )
        .route(
            "/libraries/{id}/organize-preview",
            get(library_organize::preview_organize),
        )
        .route(
            "/libraries/{id}/path-reconciliation-preview",
            get(library_housekeeping::path_reconciliation_preview),
        )
        .route(
            "/libraries/{id}/root-consolidation-preview",
            get(library_housekeeping::root_consolidation_preview),
        )
        .route(
            "/libraries/{id}/items/{item_id}/reidentification-preview",
            get(reidentify::preview),
        )
        .route("/unidentified", get(library_admin::list_unidentified))
        .route(
            "/libraries/{id}/duplicates",
            get(library_duplicates::duplicates),
        )
        .route(
            "/libraries/{id}/path-reconciliations",
            post(library_housekeeping::path_reconciliations),
        )
        .route(
            "/libraries/{id}/root-consolidations",
            post(library_housekeeping::root_consolidations),
        )
        .route("/libraries/{id}/organize", post(library_organize::organize))
}

pub(super) fn admin_metadata_routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/libraries/{id}/metadata/refresh",
            post(library_scan::refresh_metadata),
        )
        .route(
            "/libraries/{id}/items/{item_id}/metadata/refresh",
            post(library_organize::refresh_item_metadata),
        )
        .route(
            "/libraries/{id}/items/{item_id}/probe",
            post(library::probe_item),
        )
        .route(
            "/libraries/{id}/items/relax-filter",
            post(library_organize::relax_filter),
        )
        .route("/reidentify", post(reidentify::reidentify))
        .route("/reidentify/extras", post(reidentify::extras))
        .route(
            "/libraries/{id}/items/{item_id}/chapters/generate",
            post(library_chapters::generate_chapters),
        )
        .route(
            "/libraries/{id}/items/{item_id}/chapters/refresh",
            post(library_chapters::refresh_item_chapters),
        )
        .route(
            "/libraries/{id}/items/{item_id}/probe-status",
            get(library_chapters::probe_status),
        )
        .route(
            "/libraries/{id}/cover",
            post(library_artwork::upload_library_cover)
                .delete(library_artwork::delete_library_cover),
        )
        .route(
            "/libraries/{id}/cover/generate",
            post(library_artwork::generate_library_cover),
        )
        .route(
            "/libraries/{id}/items/{item_id}/artwork/select",
            post(library_artwork::select_artwork),
        )
        .route(
            "/libraries/{id}/items/{item_id}/artwork/upload",
            post(library_artwork::upload_artwork),
        )
        .route(
            "/unidentified/{id}/claim",
            post(library_admin::claim_unidentified),
        )
        .route(
            "/unidentified/claim",
            post(crate::claim::claim_unidentified_v1),
        )
        .route(
            "/catalog/cache",
            delete(crate::catalog_cache::delete_catalog_cache),
        )
        .route(
            "/media/{id}/metadata-overrides",
            put(media::put_metadata_overrides),
        )
        .route("/fs/browse", get(fs::browse_fs))
        // ledger: 全局台账查看与删除是敏感文件操作，仅管理员可用
        .route("/ledger", get(library_admin::list_ledger))
        .route(
            "/ledger/{id}",
            axum::routing::delete(library_admin::delete_ledger_row),
        )
        .route(
            "/ledger/{id}/retransfer",
            post(library_admin::retransfer_ledger_row),
        )
}

pub(super) fn admin_ops_routes() -> Router<ApiState> {
    Router::new()
        // jobs：任务执行与启停
        .route("/jobs/{id}", patch(jobs::toggle_job))
        .route("/jobs/{id}/schedule", put(jobs::update_job_schedule))
        // DB8 维护配置与只读预览（仅管理员；会员在 admin 中间件层 403）。
        .route(
            "/jobs/{id}/maintenance-config",
            put(job_maintenance::update_maintenance_config),
        )
        .route(
            "/jobs/{id}/maintenance-preview",
            post(job_maintenance::preview_maintenance),
        )
        .route("/jobs/{id}/run", post(jobs::run_job))
        .route("/jobs/{id}/cancel", post(jobs::cancel_job))
        .route("/jobs/tick", post(jobs::tick_jobs))
        .route("/search/admit", post(crate::admit::admit_torrent_v1))
        // media merges：202 受理 + GET 轮询持久阶段。GET 必须保持读可用，
        // 因此两条都在 write_gate 只管非读请求的 admin_ops_routes() 内。
        .route("/media/merges", post(media_operations::post_merge))
        .route(
            "/media/operations/{id}",
            get(media_operations::get_operation),
        )
        .route("/playback/policy", put(playback::put_policy))
        // directory + settings + notify（写入）
        .route("/directory", put(directory::put_directory))
        .route("/directory/roots", post(directory::add_root))
        .route("/directory/roots/{id}", delete(directory::delete_root))
        .route("/notify/config", put(notify::put_config))
        .route("/notify/test", post(notify::test))
        .route(
            "/settings/metadata",
            get(settings::get_metadata).put(settings::put_metadata),
        )
        .route("/settings/metadata/test", post(settings::test_metadata))
        .route(
            "/settings/proxy",
            get(settings::get_proxy).put(settings::put_proxy),
        )
        .route("/settings/proxy/test", post(settings::test_proxy))
        .route("/settings/proxy/diagnose", post(settings::diagnose_proxy))
        .route(
            "/settings/scrape",
            put(scrape_settings::put_scrape_settings),
        )
        // Browser settings/status expose the CDP endpoint, managed port and
        // binary path (a local CDP port can drive the browser): admin-only.
        .route(
            "/settings/browser",
            get(settings::get_browser_settings).put(settings::put_browser_settings),
        )
        .route(
            "/settings/browser/obscura",
            get(settings::get_obscura_status),
        )
        .route(
            "/settings/browser/obscura/start",
            post(settings::post_obscura_start),
        )
        .route(
            "/settings/browser/obscura/stop",
            post(settings::post_obscura_stop),
        )
        .route(
            "/settings/browser/sync-cdp",
            post(settings::sync_cdp_cookies),
        )
        // system logs (admin-only)
        .route("/system/logs", get(system_logs::list_logs))
        .route("/system/logs/stream", get(system_logs::stream_logs))
        .route("/system/logs/clear", post(system_logs::clear_logs))
        .route("/system/logs/export", get(system_logs::export_logs))
}
