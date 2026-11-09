use chrono::Utc;
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, Level};
use tracing_subscriber::EnvFilter;
use twitch_points_miner::analytics::Analytics;
use twitch_points_miner::auth::TwitchAuth;
use twitch_points_miner::bet::EventPredictionRecord;
use twitch_points_miner::chat::{ChatPool, ChatPresence};
use twitch_points_miner::config::{AccountConfig, AppConfig, FollowersOrder};
use twitch_points_miner::entities::{Campaign, SharedStreamer, Streamer};
use twitch_points_miner::error::{MinerError, Result};
use twitch_points_miner::logger::SessionLogger;
use twitch_points_miner::notify::{self, Notifier, WebhookMethod};
use twitch_points_miner::settings::{millify_signed, MinerSettings, Priority};
use twitch_points_miner::twitch::gql::GqlHeaders;
use twitch_points_miner::twitch::{self, TwitchClient};
use twitch_points_miner::ws::WebSocketPool;
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(
    name = "twitch-points-miner",
    about = "Rust port of Twitch Channel Points Miner v2"
)]
struct Args {
    /// Load all settings from a JSON file (used by the desktop app)
    #[arg(long)]
    config: Option<PathBuf>,

    /// Twitch username
    #[arg(short, long)]
    username: Option<String>,

    /// Streamers to mine (comma separated)
    #[arg(short, long, value_delimiter = ',')]
    streamers: Vec<String>,

    /// Blacklisted streamers to ignore (comma separated)
    #[arg(long, value_delimiter = ',')]
    blacklist: Vec<String>,

    /// Also load and mine followed channels
    #[arg(long, default_value_t = false)]
    followers: bool,

    /// Order for followed channels: asc or desc
    #[arg(long, default_value = "asc")]
    followers_order: String,

    /// Claim drops from inventory on startup
    #[arg(long, default_value_t = true)]
    claim_drops: bool,

    /// Enable analytics HTTP dashboard and JSON series persistence
    #[arg(long, default_value_t = true)]
    analytics: bool,

    /// Analytics bind address
    #[arg(long, default_value = "127.0.0.1:9100")]
    analytics_bind: String,

    /// Analytics auto-refresh interval in minutes
    #[arg(long, default_value_t = 5)]
    analytics_refresh: u64,

    /// Default days shown in analytics chart
    #[arg(long, default_value_t = 7)]
    analytics_days_ago: u64,

    /// IRC chat presence: never, online, offline, always
    #[arg(long, default_value = "online")]
    chat: String,

    /// Disable TLS certificate verification (use at your own risk)
    #[arg(long, default_value_t = false)]
    disable_ssl_verify: bool,

    /// Match nickname mentions in chat even without '@' prefix
    #[arg(long, default_value_t = false)]
    disable_at_in_nickname: bool,

    /// Concise log formatting (`less = true`)
    #[arg(long, default_value_t = false)]
    less: bool,

    /// Suppress emojis in log output
    #[arg(long, default_value_t = false)]
    no_emoji: bool,

    /// Disable ANSI colors in console output
    #[arg(long, default_value_t = false)]
    no_color: bool,

    /// Disable saving logs to disk
    #[arg(long, default_value_t = false)]
    no_save_logs: bool,

    /// Timezone for log timestamps (e.g. "+03:00" or "Europe/Moscow")
    #[arg(long, default_value = "")]
    time_zone: String,

    /// Telegram bot token
    #[arg(long)]
    telegram_token: Option<String>,

    /// Telegram chat id
    #[arg(long)]
    telegram_chat: Option<String>,

    /// Send Telegram notifications silently
    #[arg(long, default_value_t = false)]
    telegram_silent: bool,

    /// Discord webhook URL
    #[arg(long)]
    discord_webhook: Option<String>,

    /// Gotify base URL or endpoint
    #[arg(long)]
    gotify_url: Option<String>,

    /// Gotify app token
    #[arg(long)]
    gotify_token: Option<String>,

    /// Gotify message priority
    #[arg(long, default_value_t = 5)]
    gotify_priority: i32,

    /// Generic HTTP webhook URL
    #[arg(long)]
    webhook: Option<String>,

    /// Webhook HTTP method: GET or POST
    #[arg(long, default_value = "POST")]
    webhook_method: String,

    /// Matrix homeserver (e.g. matrix.org)
    #[arg(long)]
    matrix_homeserver: Option<String>,

    /// Matrix room ID
    #[arg(long)]
    matrix_room: Option<String>,

    /// Matrix username
    #[arg(long)]
    matrix_user: Option<String>,

    /// Matrix password
    #[arg(long)]
    matrix_password: Option<String>,

    /// Pushover user key
    #[arg(long)]
    pushover_user: Option<String>,

    /// Pushover application token
    #[arg(long)]
    pushover_token: Option<String>,

    /// Pushover priority (-2..=2)
    #[arg(long, default_value_t = 0)]
    pushover_priority: i32,

    /// Pushover sound name
    #[arg(long, default_value = "pushover")]
    pushover_sound: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive(Level::WARN.into())
                .add_directive("tungstenite=warn".parse().unwrap())
                .add_directive("tokio_tungstenite=warn".parse().unwrap())
                .add_directive("reqwest=warn".parse().unwrap()),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    let force_no_color = args.no_color || std::env::var_os("NO_COLOR").is_some();
    let force_no_emoji = args.no_emoji;
    let force_less = args.less;
    let force_no_save = args.no_save_logs;

    let mut app_config = match args.config.clone() {
        Some(path) => AppConfig::load(&path)
            .map_err(|e| MinerError::Auth(format!("Could not load config: {e}")))?,
        None => build_cli_config(args),
    };

    if force_no_color {
        app_config.logger.colored = false;
        for acc in &mut app_config.accounts {
            acc.logger.colored = false;
        }
    }
    if force_no_emoji {
        app_config.logger.emoji = false;
        for acc in &mut app_config.accounts {
            acc.logger.emoji = false;
        }
    }
    if force_less {
        app_config.logger.less = true;
        for acc in &mut app_config.accounts {
            acc.logger.less = true;
        }
    }
    if force_no_save {
        app_config.logger.save = false;
        for acc in &mut app_config.accounts {
            acc.logger.save = false;
        }
    }

    run_miner_app(app_config).await
}

fn build_cli_config(args: Args) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.username = args.username.unwrap_or_default();
    cfg.streamers = args.streamers;
    cfg.blacklist = args.blacklist;
    cfg.followers = args.followers;
    cfg.followers_order = if args.followers_order.eq_ignore_ascii_case("desc") {
        FollowersOrder::Descending
    } else {
        FollowersOrder::Ascending
    };
    cfg.claim_drops = args.claim_drops;
    cfg.analytics = args.analytics;
    cfg.analytics_bind = args.analytics_bind;
    cfg.analytics_refresh_minutes = args.analytics_refresh;
    cfg.analytics_days_ago = args.analytics_days_ago;
    cfg.chat = match args.chat.to_lowercase().as_str() {
        "never" => ChatPresence::Never,
        "always" => ChatPresence::Always,
        "offline" => ChatPresence::Offline,
        _ => ChatPresence::Online,
    };
    cfg.miner.claim_drops_startup = args.claim_drops;
    cfg.miner.disable_ssl_cert_verification = args.disable_ssl_verify;
    cfg.miner.disable_at_in_nickname = args.disable_at_in_nickname;

    cfg.logger.less = args.less;
    if args.no_emoji {
        cfg.logger.emoji = false;
    }
    if args.no_color {
        cfg.logger.colored = false;
    }
    if args.no_save_logs {
        cfg.logger.save = false;
    }
    cfg.logger.time_zone = args.time_zone;

    if let (Some(token), Some(chat)) = (args.telegram_token, args.telegram_chat) {
        cfg.notifications.telegram = Some(notify::TelegramConfig {
            bot_token: token,
            chat_id: chat,
            disable_notification: args.telegram_silent,
            ..Default::default()
        });
    }
    if let Some(url) = args.discord_webhook {
        cfg.notifications.discord = Some(notify::DiscordConfig {
            webhook_url: url,
            ..Default::default()
        });
    }
    if let Some(url) = args.webhook {
        let method = if args.webhook_method.eq_ignore_ascii_case("get") {
            WebhookMethod::Get
        } else {
            WebhookMethod::Post
        };
        cfg.notifications.webhook = Some(notify::WebhookConfig {
            url,
            method,
            ..Default::default()
        });
    }
    if let Some(url) = args.gotify_url {
        cfg.notifications.gotify = Some(notify::GotifyConfig {
            url,
            token: args.gotify_token.unwrap_or_default(),
            priority: args.gotify_priority,
            ..Default::default()
        });
    }
    if let (Some(homeserver), Some(room_id), Some(username), Some(password)) = (
        args.matrix_homeserver,
        args.matrix_room,
        args.matrix_user,
        args.matrix_password,
    ) {
        cfg.notifications.matrix = Some(notify::MatrixConfig {
            username,
            password,
            homeserver,
            room_id,
            ..Default::default()
        });
    }
    if let (Some(userkey), Some(token)) = (args.pushover_user, args.pushover_token) {
        cfg.notifications.pushover = Some(notify::PushoverConfig {
            userkey,
            token,
            priority: args.pushover_priority,
            sound: args.pushover_sound,
            ..Default::default()
        });
    }
    cfg
}

async fn run_miner_app(app_config: AppConfig) -> Result<()> {
    let mut accounts = app_config.effective_accounts();
    if accounts.is_empty() {
        return Err(MinerError::Auth(
            "Set --username or configure at least one Twitch username in the app config".into(),
        ));
    }

    // If multiple accounts are running simultaneously, automatically enable `[username]` prefix
    if accounts.len() > 1 {
        for acc in &mut accounts {
            acc.logger.console_username = true;
        }
    }

    let mut seen = std::collections::HashSet::new();
    for account in &accounts {
        if !seen.insert(account.username.to_lowercase()) {
            return Err(MinerError::Auth(format!(
                "Account '{}' is listed more than once",
                account.username
            )));
        }
    }

    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("twitch-points-miner");

    let analytics_opt: Option<Arc<Analytics>> = if app_config.analytics {
        let analytics = Arc::new(Analytics::with_options(
            &data_dir,
            app_config.analytics_refresh_minutes,
            app_config.analytics_days_ago,
        ));
        let server = Arc::clone(&analytics);
        let bind = app_config.analytics_bind.clone();
        tokio::spawn(async move {
            server.serve(&bind).await;
        });
        Some(analytics)
    } else {
        None
    };

    if accounts.len() > 1 {
        info!("Starting {} Twitch account(s)", accounts.len());
    }

    let bind = app_config.analytics_bind.clone();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let mut workers = tokio::task::JoinSet::new();
    for account in accounts {
        let analytics = analytics_opt.clone();
        let bind = bind.clone();
        let data_dir = data_dir.clone();
        let shutdown = shutdown_rx.clone();
        workers.spawn(async move {
            let username = account.username.clone();
            if let Err(error) = run_account(account, data_dir, analytics, bind, shutdown).await {
                eprintln!("Account {} stopped with error: {}", username, error);
            }
        });
    }

    let mut running = workers.len();
    while running > 0 {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal?;
                println!("\nCTRL+C Detected! Please wait just a moment!");
                let _ = shutdown_tx.send(true);
                break;
            }
            Some(result) = workers.join_next() => {
                running -= 1;
                if let Err(error) = result {
                    eprintln!("Account task failed: {}", error);
                }
            }
        }
    }
    while let Some(result) = workers.join_next().await {
        if let Err(error) = result {
            eprintln!("Account task failed: {}", error);
        }
    }
    Ok(())
}

async fn run_account(
    account: AccountConfig,
    data_dir: PathBuf,
    analytics: Option<Arc<Analytics>>,
    analytics_bind: String,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let session_id = Uuid::new_v4().to_string();
    let start_utc = Utc::now();
    let username = account.username.clone();
    let settings = account.miner.clone();

    let username_prefix = if account.logger.console_username {
        Some(username.clone())
    } else {
        None
    };
    let notifier = Arc::new(Notifier::with_options(
        account.notifications.clone(),
        settings.disable_ssl_cert_verification,
        username_prefix,
    ));
    let logger = Arc::new(SessionLogger::new(
        &username,
        account.logger.clone(),
        &data_dir,
        Arc::clone(&notifier),
    ));

    logger.info("💣", &format!("Start session: '{session_id}'"));

    // Authenticate via Twitch TV Device Code OAuth
    let mut auth = TwitchAuth::with_options(
        &username,
        settings.disable_ssl_cert_verification,
        Some(Arc::clone(&logger)),
    )?;
    auth.login().await?;

    let token = auth
        .access_token
        .clone()
        .ok_or_else(|| MinerError::Auth("Missing access_token after login".into()))?;
    let user_id = auth
        .user_id
        .clone()
        .ok_or_else(|| MinerError::Auth("Missing user_id after login".into()))?;
    if user_id.is_empty() {
        return Err(MinerError::Auth("Empty user_id returned by Twitch".into()));
    }

    let client = Arc::new(TwitchClient::with_context(
        auth,
        Some(Arc::clone(&logger)),
        analytics.clone(),
    ));
    client.refresh_client_version().await;

    // Claim drops on startup
    if account.claim_drops && settings.claim_drops_startup {
        logger.info("🎁", "Checking inventory for claimable drops...");
        match client.claim_all_drops().await {
            Ok(n) if n > 0 => logger.info("📦", &format!("Claimed {n} drop(s) on startup")),
            Ok(_) => logger.debug("No claimable drops on startup"),
            Err(error) => logger.warn(&format!("Startup drops claim failed: {error}")),
        }
    }

    // Build streamer list (manual list + optional followed channels - blacklist), normalized & deduplicated
    let blacklist: Vec<String> = account
        .blacklist
        .iter()
        .map(|s| normalize_channel_name(s))
        .filter(|s| !s.is_empty())
        .collect();

    let mut names: Vec<String> = Vec::new();
    for raw in &account.streamers {
        let clean = normalize_channel_name(raw);
        if !clean.is_empty()
            && !is_blacklisted(&clean, &blacklist)
            && !names.iter().any(|item| item == &clean)
        {
            names.push(clean);
        }
    }
    if account.followers {
        logger.info("📋", "Loading followed channels...");
        let ver = client.current_client_version();
        let headers = GqlHeaders {
            auth_token: &token,
            device_id: &client.auth.device_id,
            client_version: &ver,
            client_session_id: Some(&client.client_session_id),
        };
        match twitch::gql::get_followers_ext(
            &client.auth.client,
            &headers,
            100,
            match account.followers_order {
                FollowersOrder::Ascending => "ASC",
                FollowersOrder::Descending => "DESC",
            },
        )
        .await
        {
            Ok(follows) => {
                logger.info("📋", &format!("Loaded {} followed channels", follows.len()));
                for raw in follows {
                    let name = normalize_channel_name(&raw);
                    if !name.is_empty()
                        && !is_blacklisted(&name, &blacklist)
                        && !names.iter().any(|item| item == &name)
                    {
                        names.push(name);
                    }
                }
            }
            Err(error) => logger.warn(&format!("Loading follows failed: {error}")),
        }
    }

    logger.info(
        "🤓",
        &format!("Loading data for {} streamers. Please wait ...", names.len()),
    );

    let mut streamers: Vec<SharedStreamer> = Vec::new();
    for name in &names {
        let delay_ms: u64 = {
            use rand::Rng;
            rand::thread_rng().gen_range(250..=550)
        };
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;

        let profile = account
            .streamer_settings
            .get(name)
            .cloned()
            .or_else(|| {
                account.streamer_settings.iter().find_map(|(k, v)| {
                    if normalize_channel_name(k) == *name {
                        Some(v.clone())
                    } else {
                        None
                    }
                })
            })
            .unwrap_or_else(|| account.default_streamer_settings.clone());

        let mut streamer = Streamer::new(name).with_settings(profile);
        match client.resolve_streamer(&mut streamer).await {
            Ok(()) => {
                if let Some(a) = &analytics {
                    a.record(
                        &username,
                        &streamer.username,
                        streamer.channel_points() as i64,
                        "Start",
                    );
                }
                streamers.push(Arc::new(streamer));
            }
            Err(MinerError::StreamerNotFound(_)) => {
                logger.info("😢", &format!("Streamer {name} does not exist"));
            }
            Err(error) => {
                logger.error(&format!("Failed to resolve {name}: {error}"));
            }
        }
    }

    if streamers.is_empty() {
        logger.warn("No valid streamers found; stopping session.");
        return Ok(());
    }

    // IRC chat pool
    let chat_presence = account.chat;
    let chat_pool = Arc::new(ChatPool::with_options(
        &username,
        &token,
        settings.disable_at_in_nickname,
        Some(Arc::clone(&logger)),
    ));
    for streamer in &streamers {
        let online = streamer.is_online();
        match streamer.settings.chat.unwrap_or(chat_presence) {
            ChatPresence::Always => chat_pool.ensure_joined(&streamer.username),
            ChatPresence::Online if online => chat_pool.ensure_joined(&streamer.username),
            ChatPresence::Offline if !online => chat_pool.ensure_joined(&streamer.username),
            _ => {}
        }
    }

    // Background Drop campaigns & inventory sync (ported from `sync_campaigns_thread`)
    let any_drops = streamers.iter().any(|s| s.settings.claim_drops);
    let campaigns_handle = if any_drops {
        let mut initial_campaigns: Vec<Campaign> = Vec::new();
        if let Err(e) = client
            .sync_campaigns_once(&streamers, true, &mut initial_campaigns)
            .await
        {
            client.log_warn(&format!("Initial campaign sync failed: {e}"));
        }
        let camp_client = Arc::clone(&client);
        let camp_streamers = streamers.clone();
        let dash_interval = settings.drops_claim_interval_secs.max(300);
        Some(tokio::spawn(async move {
            let mut cached_campaigns: Vec<Campaign> = initial_campaigns;
            let mut last_dash = std::time::Instant::now();
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            // Skip immediate first tick since we just ran initial sync above
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let refresh_dash = last_dash.elapsed() >= Duration::from_secs(dash_interval);
                if refresh_dash {
                    last_dash = std::time::Instant::now();
                }
                if let Err(e) = camp_client
                    .sync_campaigns_once(&camp_streamers, refresh_dash, &mut cached_campaigns)
                    .await
                {
                    camp_client.log_warn(&format!("Error while syncing campaigns/inventory: {e}"));
                    camp_client.wait_for_internet().await;
                }
            }
        }))
    } else {
        None
    };

    // Background stream online/offline status & IRC chat sync (never blocks minute-watched loop)
    let status_handle = {
        let status_client = Arc::clone(&client);
        let status_streamers = streamers.clone();
        let status_chat = Arc::clone(&chat_pool);
        let status_interval = Duration::from_secs(settings.status_refresh_secs.max(30));
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(status_interval);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                for streamer in &status_streamers {
                    let _ = status_client.update_stream_status(streamer).await;
                    let online = streamer.is_online();
                    match streamer.settings.chat.unwrap_or(chat_presence) {
                        ChatPresence::Always => status_chat.ensure_joined(&streamer.username),
                        ChatPresence::Online if online => {
                            status_chat.ensure_joined(&streamer.username)
                        }
                        ChatPresence::Offline if !online => {
                            status_chat.ensure_joined(&streamer.username)
                        }
                        _ => status_chat.leave(&streamer.username),
                    }
                }
            }
        })
    };

    // Background periodic ChannelPointsContext refresh on online channels (every 30m by default)
    let context_handle = {
        let ctx_client = Arc::clone(&client);
        let ctx_streamers = streamers.clone();
        let ctx_interval = Duration::from_secs(settings.context_refresh_secs.max(300));
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(ctx_interval);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                ctx_client.refresh_client_version().await;
                for streamer in &ctx_streamers {
                    if streamer.is_online() {
                        ctx_client.load_channel_points_context(streamer).await;
                    }
                }
            }
        })
    };

    // Multi-socket PubSub WebSocket pool (<=50 topics per socket)
    let ws_pool = Arc::new(WebSocketPool::with_client(
        Arc::clone(&client),
        user_id,
        username.clone(),
        Some(Arc::clone(&chat_pool)),
        chat_presence,
    ));
    for streamer in &streamers {
        ws_pool.add_streamer(Arc::clone(streamer)).await;
    }
    let ws_runner = Arc::clone(&ws_pool);
    let ws_logger = Arc::clone(&logger);
    let ws_handle = tokio::spawn(async move {
        if let Err(error) = ws_runner.run().await {
            ws_logger.error(&format!("WebSocket pool stopped: {error}"));
        }
    });

    if analytics.is_some() {
        logger.info(
            "🌐",
            &format!(
                "Miner running ({} streamers, max {} concurrent). Analytics: http://{}/",
                streamers.len(),
                settings.max_watch_concurrent,
                analytics_bind
            ),
        );
    } else {
        logger.info(
            "🦀",
            &format!(
                "Miner running ({} streamers, max {} concurrent).",
                streamers.len(),
                settings.max_watch_concurrent
            ),
        );
    }

    notifier
        .send(&format!(
            "🦀 Miner started for <b>{}</b> ({} streamers)",
            username,
            streamers.len()
        ))
        .await;

    let watch_base_secs = settings.watch_interval_secs.max(10);
    let mut idle_ticks = 0u64;

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                // Select channels by priority and send minute-watched
                let selected: Vec<SharedStreamer> = select_streamers_to_watch(&streamers, &settings)
                    .into_iter()
                    .cloned()
                    .collect();
                if selected.is_empty() {
                    idle_ticks += 1;
                    if idle_ticks % 12 == 1 {
                        logger.debug("No online streamers eligible right now...");
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(watch_base_secs)) => {}
                        _ = shutdown.changed() => { break; }
                    }
                    continue;
                }
                idle_ticks = 0;

                let per_streamer_pause =
                    (watch_base_secs as f64 / selected.len().max(1) as f64).max(2.0);

                for streamer in &selected {
                    let iter_start = std::time::Instant::now();
                    if let Err(error) = client.send_minute_watched(streamer).await {
                        logger.debug(&format!(
                            "minute-watched failed for {}: {}",
                            streamer.username, error
                        ));
                    }
                    let remaining = Duration::from_secs_f64(per_streamer_pause)
                        .saturating_sub(iter_start.elapsed());
                    if !remaining.is_zero() {
                        tokio::select! {
                            _ = tokio::time::sleep(remaining) => {}
                            _ = shutdown.changed() => { break; }
                        }
                    }
                    if *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    }

    // Graceful shutdown & Final Session Report
    chat_pool.leave_all();
    ws_handle.abort();
    status_handle.abort();
    context_handle.abort();
    if let Some(ch) = campaigns_handle {
        ch.abort();
    }

    let predictions = ws_pool.prediction_records().await;
    print_session_report(&logger, &session_id, start_utc, &streamers, &predictions);

    notifier.send("🛑 Miner stopped").await;
    Ok(())
}

/// Ported from `TwitchChannelPointsMiner.__print_report()`
fn print_session_report(
    logger: &SessionLogger,
    session_id: &str,
    start_utc: chrono::DateTime<Utc>,
    streamers: &[SharedStreamer],
    predictions: &[EventPredictionRecord],
) {
    println!();
    logger.info("🛑", &format!("Ending session: '{session_id}'"));
    if let Some(path) = logger.log_file_path() {
        logger.info("📄", &format!("Logs file: {}", path.display()));
    }
    let elapsed = Utc::now() - start_utc;
    let total_secs = elapsed.num_seconds().max(0);
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    logger.info(
        "⌛",
        &format!("Duration {:02}:{:02}:{:02}", hours, mins, secs),
    );

    let less = logger.settings.less;
    if !less && !predictions.is_empty() {
        let confirmed: Vec<&EventPredictionRecord> = predictions
            .iter()
            .filter(|p| p.bet_confirmed || p.bet_placed)
            .collect();
        if !confirmed.is_empty() {
            println!();
            for pred in confirmed {
                logger.info("🔧", &pred.settings.summary());
                if let Some(fc) = &pred.settings.filter_condition {
                    logger.info("📌", &fc.summary());
                }
                logger.info("📊", &pred.print_recap(less));
            }
        }
    }

    println!();
    for streamer in streamers {
        if streamer.has_history() {
            let gained = streamer.channel_points() as i64 - streamer.initial_points() as i64;
            let header = if less {
                format!(
                    "{}, Total Points Gained: {}",
                    streamer.display_str(true),
                    millify_signed(gained)
                )
            } else {
                format!(
                    "{}, Total Points Gained (after farming - before farming): {}",
                    streamer.display_str(false),
                    millify_signed(gained)
                )
            };
            let indent = " ".repeat(25);
            let hist_lines = streamer
                .print_history()
                .split("; ")
                .map(|part| format!("{indent}{part}"))
                .collect::<Vec<_>>()
                .join("\n");
            logger.info("💰", &format!("{header}\n{hist_lines}"));
        }
    }
}

fn normalize_channel_name(raw: &str) -> String {
    raw.trim().trim_start_matches('@').to_lowercase()
}

fn is_blacklisted(name: &str, blacklist: &[String]) -> bool {
    blacklist
        .iter()
        .any(|blocked| blocked.eq_ignore_ascii_case(name))
}

fn select_streamers_to_watch<'a>(
    streamers: &'a [SharedStreamer],
    settings: &MinerSettings,
) -> Vec<&'a SharedStreamer> {
    let online: Vec<&SharedStreamer> = streamers
        .iter()
        .filter(|s| s.ready_for_minute_watched())
        .collect();
    if online.is_empty() {
        return vec![];
    }

    let mut selected: Vec<&SharedStreamer> = Vec::new();
    let max = settings.max_watch_concurrent.max(1);

    for prio in &settings.priority {
        if selected.len() >= max {
            break;
        }
        match prio {
            Priority::Streak => {
                for s in &online {
                    if selected.len() >= max {
                        break;
                    }
                    if s.has_active_streak() && !selected.iter().any(|x| x.username == s.username) {
                        selected.push(*s);
                    }
                }
            }
            Priority::Drops => {
                for s in &online {
                    if selected.len() >= max {
                        break;
                    }
                    if s.drops_condition() && !selected.iter().any(|x| x.username == s.username) {
                        selected.push(*s);
                    }
                }
            }
            Priority::Subscribed => {
                let mut mult_streamers: Vec<&SharedStreamer> = online
                    .iter()
                    .copied()
                    .filter(|s| s.viewer_has_points_multiplier())
                    .collect();
                mult_streamers.sort_by(|a, b| {
                    b.total_points_multiplier()
                        .partial_cmp(&a.total_points_multiplier())
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                for s in mult_streamers {
                    if selected.len() >= max {
                        break;
                    }
                    if !selected.iter().any(|x| x.username == s.username) {
                        selected.push(s);
                    }
                }
            }
            Priority::Order => {
                for s in &online {
                    if selected.len() >= max {
                        break;
                    }
                    if !selected.iter().any(|x| x.username == s.username) {
                        selected.push(*s);
                    }
                }
            }
            Priority::PointsAscending => {
                let mut sorted = online.clone();
                sorted.sort_by_key(|s| s.channel_points());
                for s in sorted {
                    if selected.len() >= max {
                        break;
                    }
                    if !selected.iter().any(|x| x.username == s.username) {
                        selected.push(s);
                    }
                }
            }
            Priority::PointsDescending => {
                let mut sorted = online.clone();
                sorted.sort_by_key(|s| std::cmp::Reverse(s.channel_points()));
                for s in sorted {
                    if selected.len() >= max {
                        break;
                    }
                    if !selected.iter().any(|x| x.username == s.username) {
                        selected.push(s);
                    }
                }
            }
        }
    }

    selected
}
