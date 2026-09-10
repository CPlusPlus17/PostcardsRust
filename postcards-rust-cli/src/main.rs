//! PostcardsRust CLI (port of `PostcardsDotnet.Cli.Program`).
//!
//! The .NET CLI:
//! * loads a plugin (GooglePhotos) from `PCDNCLI_PLUGINPATH`
//! * plugin login + sync
//! * creates the SwissPostcardCreatorApi (PCC login was a TODO)
//! * sends the next cached photo, deleting it on success
//! * then runs a sync timer (default every 600 min, `PCDNCLI_PLUGINSYNCTIME`)
//!   and a 24h send timer forever.
//!
//! The Rust port keeps the same behavior and env vars, adds an explicit PCC
//! login when `PCD_USERNAME`/`PCD_PASSWORD` are set, and exposes subcommands
//! (`sync`, `send`, `quota`, `user`, `balance`, `daemon`).

use clap::{Parser, Subcommand};
use postcards_rust_api::SwissPostcardCreatorApi;
use postcards_rust_plugin_base::ICommand;
use postcards_rust_plugin_google_photos::GooglePhotoCommand;

#[derive(Debug, Parser)]
#[command(name = "postcards-rust", version, about = "Send postcards from your Google Photos albums")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Sync new photos from the plugin into the local cache
    Sync,
    /// Send the next cached photo as a postcard
    Send,
    /// Show the PCC quota
    Quota,
    /// Show the PCC account balance
    Balance,
    /// Show PCC account information
    User,
    /// Probe which app-version header/param the API accepts (one login).
    Probe,
    /// Run forever: periodic sync + daily send (default behavior)
    Daemon,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    match cli.command.unwrap_or(Command::Daemon) {
        Command::Sync => rt.block_on(do_sync())?,
        Command::Send => rt.block_on(do_send())?,
        Command::Quota => rt.block_on(do_quota())?,
        Command::Balance => rt.block_on(do_balance())?,
        Command::User => rt.block_on(do_user())?,
        Command::Probe => rt.block_on(do_probe())?,
        Command::Daemon => rt.block_on(daemon(plugin()?))?,
    }
    Ok(())
}

/// Create the photo plugin (Google Photos, as in the .NET build).
fn plugin() -> anyhow::Result<GooglePhotoCommand> {
    GooglePhotoCommand::from_env()
}

/// Create a PCC API client, logging in when credentials are available.
async fn pcc_api() -> anyhow::Result<SwissPostcardCreatorApi> {
    let mut api = SwissPostcardCreatorApi::new();
    let username = std::env::var("PCD_USERNAME").unwrap_or_default();
    let password = std::env::var("PCD_PASSWORD").unwrap_or_default();
    if !username.is_empty() && !password.is_empty() {
        tracing::info!("authenticating PCC as {username} (cache-first)");
        api.ensure_token(&username, &password).await?;
        set_addresses(&mut api);
    }
    Ok(api)
}

/// Sync: login plugin, then sync new photos.
async fn do_sync() -> anyhow::Result<()> {
    let mut plugin = plugin()?;
    plugin.login().await?;
    plugin.sync().await?;
    Ok(())
}

/// Send: sync, then send the next cached photo.
async fn do_send() -> anyhow::Result<()> {
    let mut plugin = plugin()?;
    plugin.login().await?;
    plugin.sync().await?;
    send_one(plugin).await
}

/// Quota.
async fn do_quota() -> anyhow::Result<()> {
    let api = pcc_api().await?;
    let quota = api.get_quota().await?;
    println!(
        "quota={} available={} next={:?} end={}",
        quota.quota,
        quota.available,
        quota.next,
        quota.end
    );
    Ok(())
}

/// Balance.
async fn do_balance() -> anyhow::Result<()> {
    let api = pcc_api().await?;
    let balance = api.get_account_balance().await?;
    println!("forecastSaldo={:?}", balance.forecast_saldo);
    Ok(())
}

/// User information.
async fn do_user() -> anyhow::Result<()> {
    let api = pcc_api().await?;
    let user = api.get_user_information().await?;
    println!(
        "name={} first={} company={} street={} zip={} city={}",
        user.name, user.first_name, user.company, user.street, user.zip, user.city
    );
    Ok(())
}

/// Probe: one login, then test candidate app-version transports against /user/quota.
async fn do_probe() -> anyhow::Result<()> {
    let api = pcc_api().await?;
    let version = std::env::var("PCD_APP_VERSION").unwrap_or_else(|_| "4.38.1.0".to_string());
    let results = api.probe_app_version(&version).await?;
    for (label, status, err) in results {
        println!("{label:40} -> {status}  {err}");
    }
    Ok(())
}

/// Sender/recipient from the same env vars the .NET tests use.
fn set_addresses(api: &mut SwissPostcardCreatorApi) {
    if let (Ok(first), Ok(last), Ok(street), Ok(zip), Ok(city)) = (
        std::env::var("PCD_SENDERFIRSTNAME"),
        std::env::var("PCD_SENDERLASTNAME"),
        std::env::var("PCD_SENDERSTREET"),
        std::env::var("PCD_SENDERZIP"),
        std::env::var("PCD_SENDERCITY"),
    ) {
        api.set_sender(postcards_rust_core::types::SenderAddress {
            first_name: first,
            last_name: last,
            street,
            zip,
            city,
            company: std::env::var("PCD_SENDERCOMPANY").ok(),
        });
    }
    if let (Ok(first), Ok(last), Ok(street), Ok(zip), Ok(city)) = (
        std::env::var("PCD_RECIPIENTFIRSTNAME"),
        std::env::var("PCD_RECIPIENTLASTNAME"),
        std::env::var("PCD_RECIPIENTSTREET"),
        std::env::var("PCD_RECIPIENTZIP"),
        std::env::var("PCD_RECIPIENTCITY"),
    ) {
        api.set_recipient(postcards_rust_core::types::RecipientAddress {
            first_name: first,
            last_name: last,
            street,
            zip,
            city,
            ..Default::default()
        });
    }
}

/// Send one postcard (login PCC if creds, get next photo, send, delete).
async fn send_one(plugin: GooglePhotoCommand) -> anyhow::Result<()> {
    let mut api = pcc_api().await?;
    let next_photo = plugin.get_next_photo().await?;
    tracing::info!("sending {}", next_photo.display());
    let image = tokio::fs::read(&next_photo).await?;
    if api.send_postcard(image, Some("API Test".to_string())).await? {
        plugin.delete_cached_photo(&next_photo).await?;
        tracing::info!("postcard sent and photo removed");
    } else {
        tracing::warn!("send reported failure - photo kept");
    }
    Ok(())
}

/// The .NET `Program.Main` loop: sync every N minutes, send every 24h.
async fn daemon(mut plugin: GooglePhotoCommand) -> anyhow::Result<()> {
    let sync_minutes: u64 = std::env::var("PCDNCLI_PLUGINSYNCTIME")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);
    let send_interval = std::time::Duration::from_secs(24 * 3600);

    // Initial: login plugin, sync, try to send.
    plugin.login().await?;
    plugin.sync().await?;
    let _ = send_one(plugin.clone()).await.map_err(|e| {
        tracing::warn!("initial send failed: {e}");
    });

    let mut sync_tick = tokio::time::interval(std::time::Duration::from_secs(sync_minutes * 60));
    sync_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    sync_tick.tick().await; // first tick is immediate - skip it

    let mut send_tick = tokio::time::interval(send_interval);
    send_tick.tick().await;

    tracing::info!("daemon running: sync every {sync_minutes}m, send every 24h");
    loop {
        tokio::select! {
            _ = sync_tick.tick() => {
                if let Err(e) = plugin.sync().await {
                    tracing::warn!("sync failed: {e}");
                }
            }
            _ = send_tick.tick() => {
                if let Err(e) = send_one(plugin.clone()).await {
                    tracing::warn!("send failed: {e}");
                }
            }
        }
    }
}
