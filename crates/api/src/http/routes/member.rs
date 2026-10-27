//! Member route table (authenticated, non-admin).

use axum::Router;
use axum::routing::{delete, get, post, put};

use crate::http::{
    auth, collections, discover, downloaders, jobs, jobs_read, library, library_artwork,
    library_chapters,
    library_gallery, library_organize, library_scan, media, notify, playback, playback_activity,
    playback_logs, playback_stats, playback_views, routing_preview, rule_sets, scrape_settings,
    search, sites, subscription_depth, subscription_missing, subscriptions, title_ref,
    transfer_settings,
};
use crate::management::ApiState;

pub(super) fn member_discovery_routes() -> Router<ApiState> {
    Router::new()
        // auth
        .route("/auth/me", get(auth::me))
        // sites（只读；变更在 admin 层）
        .route("/sites", get(sites::list_sites))
        .route("/sites/{id}", get(sites::get_site))
        .route("/sites/boost-stats", get(sites::boost_stats))
        .route("/sites/catalog", get(sites::site_catalog))
        // search
        .route("/search/torrents", get(search::search_torrents))
        .route("/search/torrents/stream", get(search::search_stream))
        .route("/search/titles", get(search::search_titles))
        .route("/search/library-items", get(search::search_library_items))
        .route(
            "/search/history",
            get(search::search_history).delete(search::clear_search_history_route),
        )
        .route(
            "/search/history/{id}",
            get(search::get_history_snapshot).delete(search::delete_history_entry_route),
        )
        .route(
            "/search/presets",
            get(search::get_presets).put(search::put_presets),
        )
        .route("/discover/{kind}", get(discover::discover_kind))
        .route(
            "/discover/{kind}/filtered",
            get(discover::discover_filtered),
        )
        .route(
            "/discover/{kind}/collection/{id}",
            get(discover::discover_collection),
        )
        .route("/genres/{kind}", get(discover::genres))
        .route("/media/douban/{id}", get(media::douban_detail))
        .route("/media/{kind}/{id}", get(media::media_detail))
        .route("/media/tv/{id}/season/{season}", get(media::season_detail))
        .route("/media/person/{id}", get(media::person_detail))
        // rule-sets (Filter groups)
        .route("/rule-sets", get(rule_sets::list_rule_sets))
        .route("/rule-sets/{id}", get(rule_sets::get_rule_set))
}

pub(super) fn member_subscriptions_routes() -> Router<ApiState> {
    Router::new()
        // subscriptions（实例级归属在 handler 内校验）
        .route(
            "/subscriptions",
            get(subscriptions::list_subscriptions).post(subscriptions::create_subscription),
        )
        .route(
            "/subscriptions/{id}",
            get(subscriptions::get_subscription)
                .patch(subscriptions::patch_subscription)
                .delete(subscriptions::delete_subscription),
        )
        .route(
            "/subscriptions/{id}/run",
            post(crate::management::run_subscribe),
        )
        .route(
            "/subscriptions/{id}/search",
            post(subscriptions::run_subscription_search),
        )
        .route(
            "/subscriptions/{id}/upgrade-runs",
            post(subscription_depth::run_upgrade),
        )
        .route(
            "/subscriptions/{id}/missing-resource-searches",
            post(subscription_missing::run_missing_search),
        )
        .route(
            "/subscriptions/{id}/removal-preview",
            get(subscription_depth::removal_preview),
        )
        .route(
            "/subscriptions/{id}/season-cleanup",
            post(subscription_depth::season_cleanup),
        )
        .route(
            "/subscriptions/{id}/activities",
            get(subscription_depth::activities),
        )
        .route(
            "/subscriptions/{id}/release-forecast",
            get(subscriptions::release_forecast),
        )
        .route(
            "/subscriptions/today-arrivals",
            get(subscription_depth::today_arrivals),
        )
        .route(
            "/subscriptions/automation-readiness",
            get(subscriptions::automation_readiness),
        )
        .route("/subscriptions/title-preview", post(title_ref::preview))
        .route(
            "/subscriptions/download-routing-preview",
            post(routing_preview::preview),
        )
        // downloaders（只读 + 投递；变更在 admin 层）
        .route("/downloaders", get(downloaders::list_downloaders))
        .route("/downloaders/{id}", get(downloaders::get_downloader))
        .route("/downloaders/tasks", get(downloaders::list_tasks))
        .route("/downloaders/submit", post(downloaders::submit))
        .route("/downloaders/target-prefs", get(downloaders::target_prefs))
}

pub(super) fn member_library_routes() -> Router<ApiState> {
    Router::new()
        // library + media（读取 / 观影操作；实体增删改在 admin 层）
        .route("/libraries", get(library::list_libraries))
        .route(
            "/collections",
            get(collections::list_collections).post(collections::create_collection),
        )
        .route(
            "/collections/{id}",
            axum::routing::patch(collections::rename_collection)
                .delete(collections::delete_collection),
        )
        .route(
            "/collections/{id}/items",
            get(collections::collection_items).post(collections::add_collection_item),
        )
        .route(
            "/collections/{id}/items/{media_item_id}",
            delete(collections::remove_collection_item),
        )
        .route(
            "/collections/{id}/order",
            put(collections::reorder_collection),
        )
        .route(
            "/collections/{id}/gallery",
            get(collections::collection_gallery),
        )
        .route("/libraries/{id}/items", get(library::list_items))
        .route("/libraries/{id}/items/{item_id}", get(library::item_detail))
        .route(
            "/libraries/{id}/items/{item_id}/files",
            get(library::item_files),
        )
        .route(
            "/libraries/{id}/items/{item_id}/episodes",
            get(library::item_episodes),
        )
        .route("/libraries/{id}/missing", get(library_scan::missing))
        .route("/libraries/{id}/item-index", get(library_scan::item_index))
        .route("/libraries/{id}/facets", get(library_scan::facets))
        .route("/libraries/{id}/gallery", get(library_gallery::gallery))
        .route(
            "/libraries/{id}/items/{item_id}/similar",
            get(library_organize::similar),
        )
        .route(
            "/libraries/{id}/items/{item_id}/chapters",
            get(library_chapters::chapters),
        )
        .route(
            "/libraries/{id}/items/{item_id}/artwork/candidates",
            get(library_artwork::artwork_candidates),
        )
        // catalog cache
        .route(
            "/catalog/cache",
            get(crate::catalog_cache::list_catalog_cache),
        )
        // jobs（读取；run/cancel/toggle/tick 位于 admin 层）
        .route("/jobs", get(jobs_read::list_jobs))
        .route("/jobs/{id}", get(jobs_read::get_job))
        .route("/jobs/stream", get(jobs::job_stream))
        // directory + settings + notify（读取；写入在 admin 层）
        .route("/settings/transfer", get(transfer_settings::get))
        .route("/notify/config", get(notify::get_config))
        .route(
            "/ui/preferences",
            get(playback::get_ui_prefs).put(playback::put_ui_prefs),
        )
        .route(
            "/settings/scrape",
            get(scrape_settings::get_scrape_settings),
        )
        .route(
            "/settings/scrape/preview-naming",
            post(scrape_settings::preview_naming),
        )
        .route("/settings/languages", get(scrape_settings::languages))
        .route("/settings/countries", get(scrape_settings::countries))
}

pub(super) fn member_playback_routes() -> Router<ApiState> {
    Router::new()
        // playback views (read playback_progress + ledger)
        .route("/playback/decide", post(playback::decide))
        .route("/playback/sessions", post(playback::start_session))
        .route(
            "/playback/sessions/{play_session_id}/stream",
            get(playback::session_stream),
        )
        .route("/playback/chapters", get(playback::cached_chapters))
        .route(
            "/playback/subtitles/{ledger_id}/{index}",
            get(playback::get_subtitle_file),
        )
        .route("/playback/progress", post(playback::progress))
        .route("/playback/metrics", post(playback::metrics))
        .route("/playback/policy", get(playback::get_policy))
        .route("/playback/items/{id}", get(playback::item))
        .route("/playback/items/{id}/episodes", get(playback::episodes))
        .route(
            "/playback/marks",
            get(playback::marks).post(playback::set_marks),
        )
        .route("/playback/resume", get(playback::resume))
        .route("/playback/client-log", post(playback::client_log))
        .route("/playback/up-next", get(playback_views::up_next))
        .route("/playback/favorites", get(playback_views::favorites))
        .route(
            "/playback/favorites/gallery",
            get(playback_views::favorites_gallery),
        )
        .route(
            "/playback/history",
            get(playback_logs::history).delete(playback_logs::clear_history),
        )
        .route("/playback/hardware", get(playback::hardware))
        .route("/playback/stats/watch", get(playback_stats::watch_stats))
        .route("/playback/activity", get(playback_activity::activity))
        .route(
            "/playback/activity/sessions/{device_id}/end",
            post(playback_activity::end_session),
        )
        .route("/playback/devices", get(playback_activity::devices))
        .route(
            "/playback/devices/{device_id}",
            delete(playback_activity::revoke_device),
        )
}
