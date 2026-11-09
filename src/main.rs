use clap::Parser;
use std::sync::Arc;
use tracing::{info, Level};
use tracing_subscriber::EnvFilter;
use twitch_points_miner::analytics::Analytics;
use twitch_points_miner::auth::TwitchAuth;
use twitch_points_miner::chat::{ChatPool, ChatPresence};
use twitch_points_miner::config::{AccountConfig, AppConfig, FollowersOrder};
use twitch_points_miner::entities::Streamer;
use twitch_points_miner::error::{MinerError, Result};
use twitch_points_miner::notify::{self, Notifier};
use twitch_points_miner::settings::{MinerSettings, Priority};
use twitch_points_miner::twitch::{self, TwitchClient};
use twitch_points_miner::ws::WebSocketPool;

#[derive(Parser, Debug)]
#[command(
    name = "twitch-points-miner",
    about = "Rust port of Twitch Channel Points Miner v2"
)]
struct Args {
    /// Load all settings from a JSON file (used by the desktop app)
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Twitch username
    #[arg(short, long)]
    username: Option<String>,

    /// Streamers to mine (comma separated)
    #[arg(short, long, value_delimiter = ',')]
    streamers: Vec<String>,

    /// Also load and mine followed channels
    #[arg(long, default_value_t = false)]
    followers: bool,

    /// Claim drops from inventory on startup
    #[arg(long, default_value_t = true)]
    claim_drops: bool,

    /// Enable analytics HTTP dashboard
    #[arg(long, default_value_t = true)]
    analytics: bool,

    /// Analytics bind address
    #[arg(long, default_value = "127.0.0.1:9100")]
    analytics_bind: String,

    /// IRC chat presence: never, online, offline, always
    #[arg(long, default_value = "online")]
    chat: String,

    /// Telegram bot token
    #[arg(long)]
    telegram_token: Option<String>,

    /// Telegram chat id
    #[arg(long)]
    telegram_chat: Option<String>,

    /// Discord webhook URL
    #[arg(long)]
    discord_webhook: Option<String>,

    /// Gotify base URL
    #[arg(long)]
    gotify_url: Option<String>,

    /// Gotify app token
    #[arg(long)]
    gotify_token: Option<String>,

    /// Generic HTTP webhook URL
    #[arg(long)]
    webhook: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive(Level::INFO.into())
                .add_directive("tungstenite=warn".parse().unwrap())
                .add_directive("tokio_tungstenite=warn".parse().unwrap())
                .add_directive("reqwest=warn".parse().unwrap()),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    let mut cli_config = AppConfig::default();
    cli_config.username = args.username.unwrap_or_default();
    cli_config.streamers = args.streamers;
    cli_config.followers = args.followers;
    cli_config.claim_drops = args.claim_drops;
    cli_config.analytics = args.analytics;
    cli_config.analytics_bind = args.analytics_bind;
    cli_config.chat = match args.chat.to_lowercase().as_str() {
        "never" => ChatPresence::Never,
        "always" => ChatPresence::Always,
        "offline" => ChatPresence::Offline,
        _ => ChatPresence::Online,
    };
    if let (Some(token), Some(chat)) = (args.telegram_token, args.telegram_chat) {
        cli_config.notifications.telegram = Some(notify::TelegramConfig {
            bot_token: token,
            chat_id: chat,
        });
    }
    if let Some(url) = args.discord_webhook {
        cli_config.notifications.discord = Some(notify::DiscordConfig { webhook_url: url });
    }
    if let Some(url) = args.webhook {
        cli_config.notifications.webhook = Some(notify::WebhookConfig { url });
    }
    if let (Some(url), Some(token)) = (args.gotify_url, args.gotify_token) {
        cli_config.notifications.gotify = Some(notify::GotifyConfig { url, token });
    }
    let app_config = match args.config {
        Some(path) => AppConfig::load(&path)
            .map_err(|e| MinerError::Auth(format!("Could not load config: {e}")))?,
        None => cli_config,
    };
    if !app_config.accounts.is_empty() {
        run_multi_accounts(app_config).await?;
        return Ok(());
    }
    if app_config.username.trim().is_empty() {
        return Err(MinerError::Auth(
            "Set --username or configure a Twitch username in the app config".into(),
        ));
    }
    let username = app_config.username.clone();
    let miner_settings = app_config.miner.clone();

    info!("🦀 Twitch Channel Points Miner (Rust port)");
    info!("Session starting for user: {}", username);

    // Notifications
    let notifier = Notifier::new(app_config.notifications.clone());

    // Analytics
    let analytics = Arc::new(Analytics::new(
        dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("twitch-points-miner"),
    ));
    if app_config.analytics {
        let a = analytics.clone();
        let bind = app_config.analytics_bind.clone();
        tokio::spawn(async move {
            a.serve(&bind).await;
        });
    }

    // Auth
    let mut auth = TwitchAuth::new(&username)?;
    auth.login().await?;

    let token = auth.access_token.clone().unwrap();
    let user_id = auth.user_id.clone().unwrap();
    let device_id = auth.device_id.clone();
    let http_client = auth.client.clone();

    let client = TwitchClient::new(auth);

    // Claim drops on startup
    if app_config.claim_drops {
        info!("Checking inventory for claimable drops...");
        match client.claim_all_drops().await {
            Ok(n) if n > 0 => {
                info!("Claimed {} drops", n);
                notifier.drop_claimed(&format!("{} drops", n)).await;
            }
            Ok(_) => info!("No claimable drops"),
            Err(e) => tracing::warn!("Drops claim failed: {}", e),
        }
    }

    // Build streamer list
    let mut names = app_config.streamers.clone();
    if app_config.followers {
        info!("Loading followed channels...");
        match twitch::gql::get_followers(
            &http_client,
            &token,
            &device_id,
            100,
            match app_config.followers_order {
                FollowersOrder::Ascending => "ASC",
                FollowersOrder::Descending => "DESC",
            },
        )
        .await
        {
            Ok(follows) => {
                info!("Loaded {} followers", follows.len());
                for f in follows {
                    if !is_blacklisted(&f, &app_config.blacklist)
                        && !names.iter().any(|n| n.eq_ignore_ascii_case(&f))
                    {
                        names.push(f);
                    }
                }
            }
            Err(e) => tracing::warn!("Failed to load followers: {}", e),
        }
    }

    names.retain(|name| !is_blacklisted(name, &app_config.blacklist));
    let mut streamers = Vec::new();
    for name in &names {
        let profile = app_config
            .streamer_settings
            .get(&name.to_lowercase())
            .cloned()
            .unwrap_or_default();
        let mut s = Streamer::new(name).with_settings(profile);
        if let Err(e) = client.resolve_streamer(&mut s).await {
            tracing::error!("Failed to resolve {}: {}", name, e);
            continue;
        }
        analytics.record(&username, &s.username, s.channel_points() as i64, "Start");
        streamers.push(Arc::new(s));
    }

    if streamers.is_empty() {
        tracing::error!("No valid streamers. Use -s streamer1,streamer2 or --followers");
        return Ok(());
    }

    info!("Loaded {} streamers", streamers.len());

    // IRC chat
    let chat_presence = app_config.chat;
    let chat_pool = ChatPool::new(&username, &token);
    for s in &streamers {
        if s.settings.chat.unwrap_or(chat_presence) == ChatPresence::Always {
            chat_pool.ensure_joined(&s.username);
        }
    }

    // WebSocket
    let ws_pool = WebSocketPool::new(
        token.clone(),
        user_id,
        device_id.clone(),
        http_client.clone(),
    );
    for s in &streamers {
        ws_pool.add_streamer(s.clone()).await;
    }

    let ws_handle = tokio::spawn(async move {
        if let Err(e) = ws_pool.run().await {
            tracing::error!("WS pool stopped: {}", e);
        }
    });

    info!(
        "Miner running (max {} concurrent). Analytics: http://{}",
        miner_settings.max_watch_concurrent, app_config.analytics_bind
    );
    notifier
        .send(&format!(
            "🦀 Miner started for <b>{}</b> ({} streamers)",
            username,
            streamers.len()
        ))
        .await;

    let mut tick = 0u64;
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(
        miner_settings.watch_interval_secs,
    ));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                tick += 1;

                // Refresh online status + chat presence
                if tick % (miner_settings.status_refresh_secs / miner_settings.watch_interval_secs).max(1) == 0 {
                    for s in &streamers {
                        let _ = client.update_stream_status(s).await;
                        let online = s.is_online();
                        match s.settings.chat.unwrap_or(chat_presence) {
                            ChatPresence::Always => chat_pool.ensure_joined(&s.username),
                            ChatPresence::Online if online => chat_pool.ensure_joined(&s.username),
                            ChatPresence::Offline if !online => chat_pool.ensure_joined(&s.username),
                            _ => chat_pool.leave(&s.username),
                        }
                        // Community goals when online
                        if online {
                            let _ = client.contribute_community_goals(s).await;
                        }
                        // Record balance
                        analytics.record(&username, &s.username, s.channel_points() as i64, "Watch");
                    }
                }

                // Periodic drops claim
                if tick % (miner_settings.drops_claim_interval_secs / miner_settings.watch_interval_secs).max(1) == 0 {
                    if let Ok(n) = client.claim_all_drops().await {
                        if n > 0 {
                            notifier.drop_claimed(&format!("{} drops", n)).await;
                        }
                    }
                }

                let selected = select_streamers_to_watch(&streamers, &miner_settings);

                if selected.is_empty() {
                    if tick % 12 == 0 {
                        info!("No online streamers right now...");
                    }
                    continue;
                }

                for s in selected {
                    if let Err(e) = client.send_minute_watched(s).await {
                        tracing::debug!("minute-watched error for {}: {}", s.username, e);
                    } else {
                        analytics.record(&username, &s.username, s.channel_points() as i64, "Watch");
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Shutting down...");
                chat_pool.leave_all();
                notifier.send("🛑 Miner stopped").await;
                break;
            }
        }
    }

    ws_handle.abort();
    info!("Bye!");
    Ok(())
}

async fn run_multi_accounts(app_config: AppConfig) -> Result<()> {
    let accounts = app_config.effective_accounts();
    if accounts.is_empty() {
        return Err(MinerError::Auth("No configured Twitch accounts".into()));
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

    let analytics = Arc::new(Analytics::new(
        dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("twitch-points-miner"),
    ));
    if app_config.analytics {
        let server = Arc::clone(&analytics);
        let bind = app_config.analytics_bind.clone();
        tokio::spawn(async move {
            server.serve(&bind).await;
        });
    }

    info!("Starting {} Twitch account(s)", accounts.len());
    let bind = app_config.analytics_bind.clone();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let mut workers = tokio::task::JoinSet::new();
    for account in accounts {
        let analytics = Arc::clone(&analytics);
        let bind = bind.clone();
        let shutdown = shutdown_rx.clone();
        workers.spawn(async move {
            let username = account.username.clone();
            if let Err(error) = run_account(account, analytics, bind, shutdown).await {
                tracing::error!("Account {} stopped with error: {}", username, error);
            }
        });
    }

    let mut running = workers.len();
    while running > 0 {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal?;
                info!("Stopping all account sessions...");
                let _ = shutdown_tx.send(true);
                break;
            }
            Some(result) = workers.join_next() => {
                running -= 1;
                if let Err(error) = result { tracing::error!("Account task failed: {}", error); }
            }
        }
    }
    while let Some(result) = workers.join_next().await {
        if let Err(error) = result {
            tracing::error!("Account task failed: {}", error);
        }
    }
    Ok(())
}

async fn run_account(
    account: AccountConfig,
    analytics: Arc<Analytics>,
    analytics_bind: String,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let username = account.username.clone();
    let settings = account.miner.clone();
    let notifier = Notifier::new(account.notifications.clone());
    info!("Account {}: starting Twitch login", username);

    let mut auth = TwitchAuth::new(&username)?;
    auth.login().await?;
    let token = auth.access_token.clone().unwrap();
    let user_id = auth.user_id.clone().unwrap();
    let device_id = auth.device_id.clone();
    let http_client = auth.client.clone();
    let client = TwitchClient::new(auth);

    if account.claim_drops {
        match client.claim_all_drops().await {
            Ok(n) if n > 0 => {
                info!("Account {}: claimed {} Drops", username, n);
                notifier.drop_claimed(&format!("{} drops", n)).await;
            }
            Ok(_) => info!("Account {}: no claimable Drops", username),
            Err(error) => tracing::warn!("Account {}: claim Drops failed: {}", username, error),
        }
    }

    let mut names = account.streamers.clone();
    if account.followers {
        match twitch::gql::get_followers(
            &http_client,
            &token,
            &device_id,
            100,
            match account.followers_order {
                FollowersOrder::Ascending => "ASC",
                FollowersOrder::Descending => "DESC",
            },
        )
        .await
        {
            Ok(follows) => {
                info!(
                    "Account {}: loaded {} followed channels",
                    username,
                    follows.len()
                );
                for name in follows {
                    if !is_blacklisted(&name, &account.blacklist)
                        && !names.iter().any(|item| item.eq_ignore_ascii_case(&name))
                    {
                        names.push(name);
                    }
                }
            }
            Err(error) => tracing::warn!("Account {}: loading follows failed: {}", username, error),
        }
    }
    names.retain(|name| !is_blacklisted(name, &account.blacklist));

    let mut streamers = Vec::new();
    for name in &names {
        let profile = account
            .streamer_settings
            .get(&name.to_lowercase())
            .cloned()
            .unwrap_or_default();
        let mut streamer = Streamer::new(name).with_settings(profile);
        if let Err(error) = client.resolve_streamer(&mut streamer).await {
            tracing::error!(
                "Account {}: failed to resolve {}: {}",
                username,
                name,
                error
            );
            continue;
        }
        analytics.record(
            &username,
            &streamer.username,
            streamer.channel_points() as i64,
            "Start",
        );
        streamers.push(Arc::new(streamer));
    }
    if streamers.is_empty() {
        tracing::warn!(
            "Account {}: no valid streamers; skipping this account",
            username
        );
        return Ok(());
    }

    let chat_presence = account.chat;
    let chat_pool = ChatPool::new(&username, &token);
    for streamer in &streamers {
        if streamer.settings.chat.unwrap_or(chat_presence) == ChatPresence::Always {
            chat_pool.ensure_joined(&streamer.username);
        }
    }

    let ws_pool = WebSocketPool::new(token.clone(), user_id, device_id, http_client);
    for streamer in &streamers {
        ws_pool.add_streamer(streamer.clone()).await;
    }
    let ws_username = username.clone();
    let ws_handle = tokio::spawn(async move {
        if let Err(error) = ws_pool.run().await {
            tracing::error!("Account {} WebSocket pool stopped: {}", ws_username, error);
        }
    });

    info!(
        "Account {}: running with {} streamers; analytics at http://{}",
        username,
        streamers.len(),
        analytics_bind
    );
    notifier
        .send(&format!(
            "Miner started for <b>{}</b> ({} streamers)",
            username,
            streamers.len()
        ))
        .await;
    let mut tick = 0u64;
    let mut interval =
        tokio::time::interval(std::time::Duration::from_secs(settings.watch_interval_secs));
    loop {
        tokio::select! {
            _ = interval.tick() => {
                tick += 1;
                if tick % (settings.status_refresh_secs / settings.watch_interval_secs).max(1) == 0 {
                    for streamer in &streamers {
                        let _ = client.update_stream_status(streamer).await;
                        let online = streamer.is_online();
                        match streamer.settings.chat.unwrap_or(chat_presence) {
                            ChatPresence::Always => chat_pool.ensure_joined(&streamer.username),
                            ChatPresence::Online if online => chat_pool.ensure_joined(&streamer.username),
                            ChatPresence::Offline if !online => chat_pool.ensure_joined(&streamer.username),
                            _ => chat_pool.leave(&streamer.username),
                        }
                        if online { let _ = client.contribute_community_goals(streamer).await; }
                        analytics.record(&username, &streamer.username, streamer.channel_points() as i64, "Watch");
                    }
                }
                if tick % (settings.drops_claim_interval_secs / settings.watch_interval_secs).max(1) == 0 {
                    if let Ok(n) = client.claim_all_drops().await {
                        if n > 0 { notifier.drop_claimed(&format!("{} drops", n)).await; }
                    }
                }
                let selected = select_streamers_to_watch(&streamers, &settings);
                if selected.is_empty() {
                    if tick % 12 == 0 { info!("Account {}: no online streamers", username); }
                    continue;
                }
                for streamer in selected {
                    if let Err(error) = client.send_minute_watched(streamer).await {
                        tracing::debug!("Account {}: minute-watched failed for {}: {}", username, streamer.username, error);
                    } else {
                        analytics.record(&username, &streamer.username, streamer.channel_points() as i64, "Watch");
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    chat_pool.leave_all();
                    notifier.send("Miner stopped").await;
                    break;
                }
            }
        }
    }
    ws_handle.abort();
    info!("Account {}: session stopped", username);
    Ok(())
}

fn is_blacklisted(name: &str, blacklist: &[String]) -> bool {
    blacklist
        .iter()
        .any(|blocked| blocked.eq_ignore_ascii_case(name))
}

fn select_streamers_to_watch<'a>(
    streamers: &'a [Arc<Streamer>],
    settings: &MinerSettings,
) -> Vec<&'a Arc<Streamer>> {
    let online: Vec<&Arc<Streamer>> = streamers.iter().filter(|s| s.is_online()).collect();
    if online.is_empty() {
        return vec![];
    }

    let mut selected: Vec<&Arc<Streamer>> = Vec::new();
    let max = settings.max_watch_concurrent;

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
                    if s.settings.claim_drops && !selected.iter().any(|x| x.username == s.username)
                    {
                        let has_game = s.stream.lock().unwrap().game_id.is_some();
                        if has_game {
                            selected.push(*s);
                        }
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
            Priority::Subscribed => {
                for s in &online {
                    if selected.len() >= max {
                        break;
                    }
                    if !selected.iter().any(|x| x.username == s.username) {
                        selected.push(*s);
                    }
                }
            }
        }
    }

    selected
}
