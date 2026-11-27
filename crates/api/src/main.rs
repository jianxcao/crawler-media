use std::net::SocketAddr;
use std::sync::Arc;

use api::anilist_http::AnilistHttp;
use api::bangumi_http::BangumiHttp;
use api::douban_http::DoubanHttp;
use api::tmdb_http::TmdbHttp;
use api::tvdb_http::TvdbHttp;
use api::{
    ApiState, DownloaderEnv, DynamicDownloader, HttpFetcher, ServerConfig, Store,
    catalog::{
        AnilistCatalog, BangumiCatalog, DoubanCatalog, FanoutCatalog, TmdbCatalog, TvdbCatalog,
    },
    cli, router,
};
use downloader::Downloader;
use indexer::RegistrySnapshot;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|s| s.as_str()) == Some("plugin") {
        match plugin_cli::run(&args[1..]) {
            Ok(out) => {
                print!("{out}");
                return;
            }
            Err(error) => {
                eprintln!("{error}");
                let code = match error {
                    plugin_cli::CliError::Usage(_) => 2,
                    _ => 1,
                };
                std::process::exit(code);
            }
        }
    }
    let command = match cli::Command::parse(&args) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    if command != cli::Command::Serve {
        let transport = match cli::HttpTransport::from_env() {
            Ok(transport) => transport,
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(2);
            }
        };
        match cli::run(command, &transport).await {
            Ok(out) => print!("{out}"),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if let Err(error) = serve().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn init_logging(
    logs_dir: &std::path::Path,
) -> (
    api::system_logs::LogBuffer,
    tracing_appender::non_blocking::WorkerGuard,
) {
    let log_buffer = api::system_logs::LogBuffer::new(api::system_logs::DEFAULT_CAPACITY);
    let buffer_layer = api::system_logs::LogBufferLayer::new(log_buffer.clone());
    let file_appender = tracing_appender::rolling::daily(logs_dir, "crawler-media.log");
    let (non_blocking_file, guard) = tracing_appender::non_blocking(file_appender);

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let mut env_filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new(
                "info,api=debug,crawler_media=debug,domain=debug,indexer=debug,media=debug,\
             downloader=debug,library=debug,subscribe=debug,filter=debug,release=debug,\
             hooks=debug,jobs=debug,playback=debug",
            )
        });
    for spec in ["html5ever=off", "selectors=warn"] {
        if let Ok(directive) = spec.parse() {
            env_filter = env_filter.add_directive(directive);
        }
    }

    let timer = api::compact_time::CompactTime;
    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_timer(timer))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking_file)
                .with_ansi(false)
                .with_timer(timer),
        )
        .with(buffer_layer)
        .init();
    (log_buffer, guard)
}

fn seed_store_settings(store: &Store, config: &ServerConfig) {
    if let Some(key) = &config.tmdb_key {
        let _ = store.put_setting(api::settings_keys::TMDB_API_KEY, key);
    }
    if let Some(proxy) = &config.metadata_proxy {
        let _ = store.put_setting(api::settings_keys::PROXY_METADATA, proxy);
    }
    if let Some(user) = &config.metadata_proxy_user {
        let _ = store.put_setting(api::settings_keys::PROXY_USERNAME, user);
    }
    if let Some(pass) = &config.metadata_proxy_pass {
        let _ = store.put_setting(api::settings_keys::PROXY_PASSWORD, pass);
    }
    if store
        .get_setting(api::settings_keys::CDP_ENABLED)
        .ok()
        .flatten()
        .is_none()
    {
        let val = if config.browser_enabled { "1" } else { "0" };
        let _ = store.put_setting(api::settings_keys::CDP_ENABLED, val);
    }
    api::http_agent::sync_from_store(store);
    api::user_agent::sync_user_agent(store);
}

fn load_catalog_sources(
    state: &ApiState,
    tvdb_key: Option<&str>,
) -> Vec<Arc<dyn api::catalog::Catalog>> {
    use media::response_cache::CacheSource;
    let mut sources: Vec<Arc<dyn api::catalog::Catalog>> = Vec::new();
    let store = state.store();
    let metadata_language = store
        .metadata_language()
        .ok()
        .map(|state| state.language)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "zh-CN".to_string());
    let tmdb =
        media::Tmdb::with_cache(TmdbHttp::new(state), state.catalog_cache(CacheSource::Tmdb));
    sources.push(TmdbCatalog::new(tmdb.with_language(&metadata_language)));
    sources.push(DoubanCatalog::new(media::Douban::with_cache(
        DoubanHttp,
        state.catalog_cache(CacheSource::Douban),
    )));
    let effective_tvdb_key = tvdb_key
        .map(str::to_string)
        .or_else(|| {
            store
                .get_setting(api::settings_keys::TVDB_API_KEY)
                .ok()
                .flatten()
        })
        .filter(|key| !key.trim().is_empty());
    if let Some(key) = effective_tvdb_key {
        sources.push(TvdbCatalog::new(media::Tvdb::with_cache(
            TvdbHttp::new(key),
            state.catalog_cache(CacheSource::Tvdb),
        )));
    }
    sources.push(BangumiCatalog::new(media::Bangumi::with_cache(
        BangumiHttp,
        state.catalog_cache(CacheSource::Bangumi),
    )));
    sources.push(AnilistCatalog::new(media::Anilist::with_cache(
        AnilistHttp,
        state.catalog_cache(CacheSource::Anilist),
    )));
    sources
}

fn spawn_background_services(
    state: &ApiState,
    address: SocketAddr,
    initial_lease: std::io::Result<Option<Arc<api::instance_lease::InstanceLease>>>,
) -> (
    api::job_loop::JobLoopHandle,
    api::PluginEventDispatcherHandle,
) {
    tokio::spawn(api::ssdp::run(
        address.port(),
        state.jellyfin_server_id().to_string(),
    ));
    let jobs = api::spawn_job_loop_with_lease(state.clone(), initial_lease);
    api::spawn_fs_watcher(state.clone());
    let events = api::spawn_plugin_event_dispatcher_with_handle(state.clone());
    (jobs, events)
}

/// Review N-2: resolve on SIGTERM (docker stop/restart) or SIGINT (Ctrl-C).
async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = interrupt => {}
        _ = terminate => {}
    }
    tracing::info!("收到退出信号，停止后台任务并释放实例锁");
}

fn create_api_state(
    config: &ServerConfig,
    data_dir: &std::path::Path,
    overlay_dir: &std::path::Path,
    store: std::sync::Arc<Store>,
    action: api::bootstrap_credentials::AdminBootstrapAction,
    log_buffer: api::system_logs::LogBuffer,
    initial_writable: bool,
) -> Result<ApiState, Box<dyn std::error::Error>> {
    let downloader: Arc<dyn Downloader> = Arc::new(DynamicDownloader::new(
        store.clone(),
        DownloaderEnv::from_server_config(config),
        data_dir,
    ));
    let listen_addr: SocketAddr = config.listen.parse()?;
    let browser = api::runtime_browser::production_browser(store.clone())?;
    let mut state = ApiState::new_arc_with_bootstrap_options(
        store.clone(),
        RegistrySnapshot::load_with_overlay(Some(overlay_dir))?,
        Arc::new(HttpFetcher::with_store_and_self_listen(
            store.clone(),
            listen_addr,
        )),
        downloader,
        action,
        api::ApiStateBootstrapOptions {
            log_buffer: Some(log_buffer),
            browser: Some(Arc::new(browser)),
            self_listen: Some(listen_addr),
            indexer_overlay_dir: Some(overlay_dir.to_path_buf()),
            initial_writable,
            pause_background_probe: true,
        },
    )?;

    let sources = load_catalog_sources(&state, config.tvdb_key.as_deref());
    if !sources.is_empty() {
        state = state.with_catalog(FanoutCatalog::new(sources));
    }
    Ok(state)
}

async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    // 先只按 env 定位数据目录并起日志，再解析完整配置：这样 `from_env` 里
    // 未识别变量的 warning 才经过 tracing，而不是丢在没有 subscriber 的空档里。
    let data_dir = api::config::data_dir_from_env();
    let logs_dir = data_dir.join("logs");
    std::fs::create_dir_all(&logs_dir)?;

    let (log_buffer, _guard) = init_logging(&logs_dir);
    let config = ServerConfig::from_env();
    tracing::info!("crawler-media 服务启动中...");

    let overlay_dir = data_dir.join("indexers");
    std::fs::create_dir_all(&overlay_dir)?;

    let token = config.token.clone().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "required environment variable CRAWLER_MEDIA_TOKEN is not set",
        )
    })?;

    let store = std::sync::Arc::new(Store::open(&data_dir)?);
    let initial_lease = api::instance_lease::acquire(&store);
    let initial_writable = initial_lease.is_ok();
    if initial_writable {
        seed_store_settings(&store, &config);
    } else {
        api::http_agent::sync_from_store(&store);
        api::user_agent::sync_user_agent(&store);
    }

    let action = api::bootstrap_credentials::prepare_admin_credentials(
        store.as_ref(),
        &token,
        config.admin_password.as_deref(),
        api::bootstrap_credentials::BootstrapMode::Production,
    )
    .map_err(|e| {
        tracing::error!(error = %e, "管理员凭据校验未通过，服务拒绝启动");
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            e.to_string(),
        )) as Box<dyn std::error::Error>
    })?;

    let state = create_api_state(
        &config,
        &data_dir,
        &overlay_dir,
        store.clone(),
        action,
        log_buffer,
        initial_writable,
    )?;

    let address: SocketAddr = config.listen.parse()?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    println!("crawler-media 已监听 http://{address}");

    let (jobs, plugin_events) = spawn_background_services(&state, address, initial_lease);

    let app = api::ui::attach_ui(router(state.clone()), config.ui_dir.clone());
    // Graceful shutdown waits for open connections (SSE streams can stay
    // open); give them 10 s after the signal, then stop anyway.
    let (signalled_tx, signalled_rx) = tokio::sync::watch::channel(false);
    let http_signal = signalled_tx.clone();
    let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
        shutdown_signal().await;
        marker::media_read::stop_remote_reads();
        http_signal.send_replace(true);
    });
    let mut grace_signal = signalled_rx.clone();
    let grace = async move {
        let _ = grace_signal.wait_for(|signalled| *signalled).await;
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    };
    let http = async {
        let served = tokio::select! {
            result = serve => result,
            _ = grace => {
                tracing::warn!("连接未在 10 秒内关闭，强制退出");
                Ok(())
            }
        };
        // Also stop workers if the HTTP server exits on its own.
        marker::media_read::stop_remote_reads();
        signalled_tx.send_replace(true);
        served
    };
    // Review N-2: stopping the job loop drops its instance lease, which
    // deletes the lock row, so an immediate restart is writable.
    let chapter_frames = state.chapter_frames();
    let shutdown = async {
        let mut signalled_rx = signalled_rx;
        let _ = signalled_rx.wait_for(|signalled| *signalled).await;
        let stop_workers = async {
            let (jobs, events, probe, frames) = tokio::join!(
                jobs.shutdown(),
                plugin_events.shutdown(),
                state.probe.shutdown_workers(),
                chapter_frames.shutdown(),
            );
            for error in [jobs, events, probe, frames]
                .into_iter()
                .filter_map(Result::err)
            {
                tracing::warn!(%error, "停止后台任务失败");
            }
        };
        if let Err(error) =
            tokio::time::timeout(std::time::Duration::from_secs(10), stop_workers).await
        {
            tracing::warn!(%error, "停止后台任务超时");
        }
    };
    let (served, ()) = tokio::join!(http, shutdown);
    state.close_library_reads();
    served?;
    Ok(())
}
