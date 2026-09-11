//! PostcardsRust CLI: automated Swiss Post postcard sender with photo backend plugins.

mod config;
mod state;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use config::{apply_addresses, build_plugin, AppConfig, PluginCliOptions};
use postcards_rust_api::SwissPostcardCreatorApi;
use postcards_rust_plugin_base::ICommand;
use state::AppState;

#[derive(Debug, Parser)]
#[command(
    name = "postcards-rust",
    version,
    about = "Automate Swiss Postcard Creator with photo backends (Immich, Google Photos)"
)]
struct Cli {
    /// Photo backend plugin to use: "immich" or "google-photos"
    #[arg(long, short = 'p', env = "PCD_PLUGIN")]
    plugin: Option<String>,

    /// Immich instance base URL (e.g. http://localhost:2283)
    #[arg(long, env = "IMMICH_INSTANCE_URL")]
    immich_url: Option<String>,

    /// Immich API key
    #[arg(long, env = "IMMICH_API_KEY")]
    immich_api_key: Option<String>,

    /// Immich album name or UUID
    #[arg(long, env = "IMMICH_ALBUM")]
    immich_album: Option<String>,

    /// Custom media cache folder
    #[arg(long, env = "IMMICH_MEDIA_FOLDER")]
    media_folder: Option<PathBuf>,

    /// Allow self-signed or invalid SSL certificates
    #[arg(long, env = "IMMICH_INSECURE_TLS")]
    insecure_tls: bool,

    /// Path to custom config JSON file (default ~/.postcards_rust/config.json)
    #[arg(long)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Sync new photos from the configured backend album into local cache
    Sync,

    /// Send the next cached photo as a postcard (respects cooldown unless --force)
    Send {
        /// Force send immediately even if within the cooldown period
        #[arg(long, short = 'f')]
        force: bool,

        /// Custom message text for the postcard back
        #[arg(long, short = 'm')]
        message: Option<String>,
    },

    /// Run automation daemon: periodic sync + scheduled postcard sending with cooldown
    Daemon {
        /// Cooldown in days between sending postcards (default: 7)
        #[arg(long, short = 'c', env = "PCD_COOLDOWN_DAYS")]
        cooldown_days: Option<u32>,

        /// Minutes between syncing the album for new photos (default: 60)
        #[arg(long, default_value_t = 60)]
        sync_minutes: u64,

        /// Force initial send immediately on daemon startup if quota allows
        #[arg(long, short = 'f')]
        force: bool,

        /// Custom message text for the postcard back
        #[arg(long, short = 'm')]
        message: Option<String>,
    },

    /// Show Swiss Post quota and local automation cooldown status
    Quota,

    /// Show PCC account balance
    Balance,

    /// Show PCC account information
    User,

    /// List available albums in the photo backend
    Albums,

    /// Probe which app-version header/param the API accepts (one login)
    Probe,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let config = AppConfig::load(cli.config.as_deref());

    let plugin_opts = PluginCliOptions {
        plugin: cli.plugin.clone(),
        immich_url: cli.immich_url.clone(),
        immich_api_key: cli.immich_api_key.clone(),
        immich_album: cli.immich_album.clone(),
        media_folder: cli.media_folder.clone(),
        insecure_tls: cli.insecure_tls,
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    match cli.command.unwrap_or(Command::Daemon {
        cooldown_days: None,
        sync_minutes: 60,
        force: false,
        message: None,
    }) {
        Command::Sync => rt.block_on(do_sync(&plugin_opts, &config))?,
        Command::Send { force, message } => rt.block_on(do_send(&plugin_opts, &config, force, message))?,
        Command::Daemon {
            cooldown_days,
            sync_minutes,
            force,
            message,
        } => rt.block_on(do_daemon(
            &plugin_opts,
            &config,
            cooldown_days,
            sync_minutes,
            force,
            message,
        ))?,
        Command::Quota => rt.block_on(do_quota(&config))?,
        Command::Balance => rt.block_on(do_balance(&config))?,
        Command::User => rt.block_on(do_user(&config))?,
        Command::Albums => rt.block_on(do_albums(&plugin_opts, &config))?,
        Command::Probe => rt.block_on(do_probe(&config))?,
    }
    Ok(())
}

/// Create a PCC API client, logging in when credentials or token cache are available.
async fn pcc_api(config: &AppConfig) -> anyhow::Result<SwissPostcardCreatorApi> {
    let mut api = SwissPostcardCreatorApi::new();
    let username = std::env::var("PCD_USERNAME").unwrap_or_default();
    let password = std::env::var("PCD_PASSWORD").unwrap_or_default();
    api.ensure_token(&username, &password).await?;
    apply_addresses(&mut api, config);
    Ok(api)
}

/// Determine configured cooldown days (CLI arg > env > config file > default 7).
fn resolve_cooldown_days(cli_days: Option<u32>, config: &AppConfig) -> u32 {
    cli_days
        .or_else(|| {
            std::env::var("PCD_COOLDOWN_DAYS")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .or(config.cooldown_days)
        .unwrap_or(7)
}

/// Sync: login plugin, then sync new photos.
async fn do_sync(plugin_opts: &PluginCliOptions, config: &AppConfig) -> anyhow::Result<()> {
    let mut plugin = build_plugin(plugin_opts, config)?;
    println!("Connecting to photo plugin: {}...", plugin.name());
    plugin.login().await?;
    println!("Syncing photos from album...");
    plugin.sync().await?;
    println!("Sync completed successfully.");
    Ok(())
}

/// List available albums in the photo backend.
async fn do_albums(plugin_opts: &PluginCliOptions, config: &AppConfig) -> anyhow::Result<()> {
    let mut plugin = build_plugin(plugin_opts, config)?;
    println!("Connecting to photo plugin: {}...", plugin.name());
    plugin.login().await?;
    let albums = plugin.list_albums().await?;
    if albums.is_empty() {
        println!("No albums found or album listing not supported by this plugin.");
        return Ok(());
    }

    println!("\nAvailable Albums ({}) :", albums.len());
    println!("{:-<75}", "");
    println!("{:<38} {:<28} {:>6}", "Album ID", "Album Name", "Assets");
    println!("{:-<75}", "");
    for a in albums {
        println!("{:<38} {:<28} {:>6}", a.id, a.name, a.asset_count);
    }
    println!("{:-<75}\n", "");
    Ok(())
}

/// Send one postcard with cooldown and quota checks.
async fn do_send(
    plugin_opts: &PluginCliOptions,
    config: &AppConfig,
    force: bool,
    custom_message: Option<String>,
) -> anyhow::Result<()> {
    let cooldown_days = resolve_cooldown_days(None, config);
    let mut state = AppState::load();

    // 1. Check local automation cooldown
    if !force {
        if let Some(remaining) = state.remaining_cooldown(cooldown_days) {
            let last = state.last_sent_at.unwrap();
            let next = state.next_eligible_at(cooldown_days).unwrap();
            let hours = remaining.num_hours();
            let days = remaining.num_days();
            println!(
                "Cooldown active: last postcard was sent on {} UTC.\n\
                 Next eligible send date is {} UTC (in {} days, {} hours).\n\
                 Use --force to bypass this cooldown and send immediately.",
                last.format("%Y-%m-%d %H:%M:%S"),
                next.format("%Y-%m-%d %H:%M:%S"),
                days,
                hours % 24
            );
            return Ok(());
        }
    }

    // 2. Check Swiss Post quota
    let mut api = pcc_api(config).await?;
    let quota = api.get_quota().await?;
    if !quota.available {
        println!(
            "Swiss Post quota unavailable. Next free card available at: {:?}",
            quota.next
        );
        return Ok(());
    }

    // 3. Connect to plugin & sync
    let mut plugin = build_plugin(plugin_opts, config)?;
    plugin.login().await?;
    plugin.sync().await?;

    // 4. Send postcard
    let next_photo = plugin.get_next_photo().await?;
    let photo_name = next_photo
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("photo.jpg")
        .to_string();

    let message = custom_message
        .or_else(|| config.default_message.clone())
        .unwrap_or_else(|| "Automated Postcard".to_string());

    println!("Preparing to send postcard using image: {}", next_photo.display());
    let image_bytes = tokio::fs::read(&next_photo).await?;

    if api.send_postcard(image_bytes, Some(message)).await? {
        println!("Postcard successfully uploaded to Swiss Post!");
        plugin.on_photo_sent(&next_photo).await?;
        state.record_sent(&photo_name)?;
        println!(
            "Photo removed from album and state updated. Next card scheduled in {} days.",
            cooldown_days
        );
    } else {
        println!("Swiss Post API reported failure. Photo was kept in album.");
    }

    Ok(())
}

/// Daemon mode: automated periodic sync and scheduled sending respecting cooldowns and quota.
async fn do_daemon(
    plugin_opts: &PluginCliOptions,
    config: &AppConfig,
    cli_cooldown: Option<u32>,
    sync_minutes: u64,
    force_initial: bool,
    message: Option<String>,
) -> anyhow::Result<()> {
    let cooldown_days = resolve_cooldown_days(cli_cooldown, config);
    let mut plugin = build_plugin(plugin_opts, config)?;

    println!("Starting PostcardsRust daemon...");
    println!("Photo Backend: {}", plugin.name());
    println!("Cooldown Setting: {} days", cooldown_days);
    println!("Sync Interval: every {} minutes", sync_minutes);

    plugin.login().await?;
    let _ = plugin.sync().await.map_err(|e| {
        tracing::warn!("Initial sync failed: {e}");
    });

    // Check if initial send should be attempted
    if force_initial {
        println!("--force specified: attempting immediate send on startup...");
        let _ = send_single_card(&mut *plugin, config, &message).await.map_err(|e| {
            tracing::warn!("Startup send failed: {e}");
        });
    } else {
        let state = AppState::load();
        if state.remaining_cooldown(cooldown_days).is_none() {
            println!("No active cooldown: attempting send...");
            let _ = send_single_card(&mut *plugin, config, &message).await.map_err(|e| {
                tracing::warn!("Startup send failed: {e}");
            });
        }
    }

    let sync_duration = std::time::Duration::from_secs(sync_minutes * 60);
    let check_interval = std::time::Duration::from_secs(300); // check status every 5 minutes

    let mut sync_tick = tokio::time::interval(sync_duration);
    sync_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    sync_tick.tick().await; // skip initial tick

    let mut check_tick = tokio::time::interval(check_interval);
    check_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    check_tick.tick().await; // skip initial tick since startup already checked

    loop {
        tokio::select! {
            _ = sync_tick.tick() => {
                tracing::info!("Running scheduled album sync...");
                if let Err(e) = plugin.sync().await {
                    tracing::warn!("Scheduled sync error: {e}");
                }
            }
            _ = check_tick.tick() => {
                let state = AppState::load();
                if state.remaining_cooldown(cooldown_days).is_none() {
                    // Cooldown has passed! Check if Swiss Post quota is also available.
                    let api_res = pcc_api(config).await;
                    match api_res {
                        Ok(api) => {
                            match api.get_quota().await {
                                Ok(q) if q.available => {
                                    tracing::info!("Cooldown elapsed and quota available: triggering send!");
                                    let _ = plugin.sync().await;
                                    if let Err(e) = send_single_card(&mut *plugin, config, &message).await {
                                        tracing::warn!("Automated send failed: {e}");
                                        // Backoff on failure to prevent rapid retry loops
                                        tokio::time::sleep(check_interval).await;
                                    }
                                }
                                Ok(q) => {
                                    tracing::info!("Cooldown elapsed but PCC quota unavailable (next={:?})", q.next);
                                }
                                Err(e) => {
                                    tracing::warn!("Quota check failed: {e}");
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("PCC API client error: {e}");
                        }
                    }
                }
            }
        }
    }
}

/// Helper to execute a single postcard send and record state.
async fn send_single_card(
    plugin: &mut dyn ICommand,
    config: &AppConfig,
    custom_message: &Option<String>,
) -> anyhow::Result<()> {
    let mut api = pcc_api(config).await?;
    let quota = api.get_quota().await?;
    if !quota.available {
        tracing::warn!("Swiss Post quota unavailable: next={:?}", quota.next);
        return Ok(());
    }

    let next_photo = match plugin.get_next_photo().await {
        Ok(p) => p,
        Err(e) => {
            tracing::info!("No photos ready in album to send: {e}");
            return Ok(());
        }
    };

    let photo_name = next_photo
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("photo.jpg")
        .to_string();

    let message = custom_message
        .clone()
        .or_else(|| config.default_message.clone())
        .unwrap_or_else(|| "Automated Postcard".to_string());

    tracing::info!("Sending photo {}", next_photo.display());
    let image_bytes = tokio::fs::read(&next_photo).await?;

    if api.send_postcard(image_bytes, Some(message)).await? {
        tracing::info!("Postcard uploaded successfully! Unlinking from album...");
        plugin.on_photo_sent(&next_photo).await?;
        let mut state = AppState::load();
        state.record_sent(&photo_name)?;
        tracing::info!("Card send completed and state saved.");
    } else {
        tracing::warn!("Swiss Post reported failure. Photo kept.");
    }

    Ok(())
}

/// Show Swiss Post quota and local automation cooldown status.
async fn do_quota(config: &AppConfig) -> anyhow::Result<()> {
    let api = pcc_api(config).await?;
    let quota = api.get_quota().await?;
    let state = AppState::load();
    let cooldown_days = resolve_cooldown_days(None, config);

    println!("\n=== Swiss Post Quota ===");
    println!("  Quota:            {}", quota.quota);
    println!("  Available Now:    {}", if quota.available { "Yes" } else { "No" });
    println!("  API Next Card:    {}", quota.next.map(|t| t.to_rfc3339()).unwrap_or_else(|| "Immediately".to_string()));
    println!("  Quota Valid End:  {}", quota.end.map(|t| t.to_rfc3339()).unwrap_or_else(|| "None".to_string()));
    println!("  Retention Days:   {}", quota.retention_days.map(|d| d.to_string()).unwrap_or_else(|| "N/A".to_string()));

    println!("\n=== Automation Cooldown Status ===");
    println!("  Configured Interval:  {} days", cooldown_days);
    println!("  Total Cards Sent:     {}", state.total_cards_sent);
    println!("  Last Photo Sent:      {}", state.last_photo_name.as_deref().unwrap_or("None"));
    println!(
        "  Last Card Sent At:    {}",
        state
            .last_sent_at
            .map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| "Never".to_string())
    );

    match state.remaining_cooldown(cooldown_days) {
        Some(rem) => {
            let next = state.next_eligible_at(cooldown_days).unwrap();
            println!(
                "  Cooldown Active:      YES ({} days, {} hours remaining)",
                rem.num_days(),
                rem.num_hours() % 24
            );
            println!("  Next Eligible Send:   {}", next.format("%Y-%m-%d %H:%M:%S UTC"));
        }
        None => {
            println!("  Cooldown Active:      NO (Ready to send)");
            println!("  Next Eligible Send:   Ready now");
        }
    }
    println!();

    Ok(())
}

/// Balance.
async fn do_balance(config: &AppConfig) -> anyhow::Result<()> {
    let api = pcc_api(config).await?;
    let balance = api.get_account_balance().await?;
    println!("forecastSaldo={:?}", balance.forecast_saldo);
    Ok(())
}

/// User information.
async fn do_user(config: &AppConfig) -> anyhow::Result<()> {
    let api = pcc_api(config).await?;
    let user = api.get_user_information().await?;
    println!(
        "name={} first={} company={} street={} zip={} city={}",
        user.name,
        user.first_name,
        user.company.as_deref().unwrap_or(""),
        user.street,
        user.zip,
        user.city
    );
    Ok(())
}

/// Probe app version.
async fn do_probe(config: &AppConfig) -> anyhow::Result<()> {
    let api = pcc_api(config).await?;
    let version = std::env::var("PCD_APP_VERSION").unwrap_or_else(|_| "4.38.1.0".to_string());
    let results = api.probe_app_version(&version).await?;
    for (label, status, err) in results {
        println!("{label:40} -> {status}  {err}");
    }
    Ok(())
}
