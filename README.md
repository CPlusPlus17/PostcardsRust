# PostcardsRust

Automated Swiss Postcard Creator CLI & daemon in Rust. Synchronizes photos from a self-hosted **Immich** album (or **Google Photos**), formats and scales them to Swiss Post specifications, and sends genuine physical postcards via the [Swiss Post Postcard Creator](https://postcardcreator.post.ch) API.

When using **Immich**, sent photos are **automatically removed from the sending album** (while remaining safely in your Immich photo library) so you can continuously queue up cards and let the daemon send them automatically.

---

## Key Features

- **📸 Photo Backend Plugins**:
  - **Immich (Recommended)**: Resolves albums by name or UUID, downloads original photos, and automatically unlinks sent photos from the album upon delivery confirmation (`DELETE /api/albums/{id}/assets`).
  - **Google Photos**: Syncs photos from designated Google Photos albums into a local cache.
- **📮 Swiss Post API (PCC)**:
  - Reverse-engineered against official PostCard Creator APK `ch.post.it.pcc` (v4.38.1.0).
  - Passes Swiss Post's `appVersionValidation` gate (`PCCApp-Version: 4.38.1.0`, `PCCApp-OS: Android`).
  - Automated SwissID OAuth flow with cache-first token reuse in `~/.postcards_rust/token.json` (no repeated 2FA prompts).
- **⏱️ 7-Day Cooldown & State Tracking**:
  - Automatically respects the Swiss Post 7-day free card retention cooldown.
  - Persistent state in `~/.postcards_rust/state.json` survives restarts and upgrades.
  - Interactive countdowns in `postcards-rust quota` and `--force` override option in `send`.
- **🚀 Flexible Deployment**:
  - Single static binary CLI & daemon.
  - Multi-stage minimal Dockerfile (`debian:bookworm-slim`, non-root user).
  - Production-ready Kubernetes **Helm Chart** supporting both `daemon` (Deployment) and `cronjob` (Kubernetes CronJob) modes with persistent PVC storage and zero-2FA token seeding.

---

## Crates Overview

| Crate | Counterpart | Purpose |
|---|---|---|
| `postcards-rust-core` | `PostcardsDotnet.Common` / `Services` | SwissID OAuth, PCC REST API client, image scaling |
| `postcards-rust-api` | `PostcardsDotnet.API` | High-level facade with automatic token caching & refresh |
| `postcards-rust-plugin-base` | `PostcardsDotnet.PluginBase` | `ICommand` trait, `AlbumSummary` model |
| `postcards-rust-plugin-immich` | New | Immich API client, album sync, and post-send unlinking |
| `postcards-rust-plugin-google-photos` | `PostcardsDotnet.PluginGooglePhotos` | Google Photos album synchronization |
| `postcards-rust-cli` | `PostcardsDotnet.Cli` | CLI application, automation daemon, cooldown manager |

---

## Building Locally

### Prerequisites
- Rust 1.85+ / 1.97
- OpenSSL & system `libcurl` 8.x development packages:
  - **Debian / Ubuntu**: `sudo apt install libssl-dev libcurl4-openssl-dev pkg-config`
  - **Fedora / RHEL**: `sudo dnf install openssl-devel libcurl-devel pkg-config`

### Build & Test
```sh
cargo build --release
cargo test --workspace
```

> **Note on system libcurl**:
> SwissID's Cloudflare / WAF validates TLS ClientHello fingerprints. The `curl` crate links against system `libcurl` to ensure successful handshakes. If you have custom library paths, prepend `LD_LIBRARY_PATH`:
> ```sh
> export LD_LIBRARY_PATH=/usr/lib/x86_64-linux-gnu:$LD_LIBRARY_PATH
> ```

---

## Configuration

PostcardsRust supports configuration via:
1. **JSON Configuration File** (`~/.postcards_rust/config.json` or `--config <path>`)
2. **Environment Variables**
3. **CLI Arguments & Flags**

### Recommended: `~/.postcards_rust/config.json`

```json
{
  "plugin": "immich",
  "cooldown_days": 7,
  "default_message": "Automated postcard greeting!",
  "immich": {
    "instance_url": "https://photos.yourdomain.com",
    "api_key": "YOUR_IMMICH_API_KEY",
    "album": "Postcards",
    "insecure_tls": false
  },
  "sender": {
    "first_name": "Max",
    "last_name": "Muster",
    "street": "Bahnhofstrasse 10",
    "zip": "8001",
    "city": "Zürich"
  },
  "recipient": {
    "first_name": "Grandma",
    "last_name": "Muster",
    "street": "Musterstrasse 1",
    "zip": "3000",
    "city": "Bern",
    "country": "SWITZERLAND"
  }
}
```

### Environment Variables Reference

#### Immich Backend
```sh
export IMMICH_INSTANCE_URL="https://photos.yourdomain.com"
export IMMICH_API_KEY="your-api-key"
export IMMICH_ALBUM="Postcards"           # Album name or UUID
# export IMMICH_MEDIA_FOLDER=~/.postcards_rust/immich_photos # Custom cache folder
# export IMMICH_INSECURE_TLS=true         # Set true for self-signed certificates
```

#### Immich API Key Permissions
When generating an API Key in Immich (**Account Settings** → **API Keys**):
- **Full Access** *(Recommended)*: Simplest setup if your Immich deployment does not require fine-grained scoping.
- **Scoped Permissions**: If using restricted permissions, ensure the API key includes the following scopes:
  - `asset.view` (or `asset.read`): Required to download high-resolution JPEG/WebP previews for HEIC/RAW assets. *(Without this, Immich returns `HTTP 403 Forbidden - Missing required permission: asset.view`)*
  - `asset.download`: Required to download original photo assets.
  - `album.read`: Required to list and inspect album contents.
  - `album.deleteAsset`: Required to automatically unlink/remove sent photos from the album (`DELETE /api/albums/{id}/assets`).

#### Swiss Post (PCC) Credentials
```sh
export PCD_USERNAME="your-swissid-email"
export PCD_PASSWORD="your-swissid-password"
```

#### Sender & Recipient (if not using config.json)
```sh
export PCD_SENDERFIRSTNAME="Max"
export PCD_SENDERLASTNAME="Muster"
export PCD_SENDERSTREET="Bahnhofstrasse 10"
export PCD_SENDERZIP="8001"
export PCD_SENDERCITY="Zürich"

export PCD_RECIPIENTFIRSTNAME="Grandma"
export PCD_RECIPIENTLASTNAME="Muster"
export PCD_RECIPIENTSTREET="Musterstrasse 1"
export PCD_RECIPIENTZIP="3000"
export PCD_RECIPIENTCITY="Bern"
export PCD_RECIPIENTCOUNTRY="SWITZERLAND"
```

---

## CLI Usage

```
Automate Swiss Postcard Creator with photo backends (Immich, Google Photos)

Usage: postcards-rust [OPTIONS] [COMMAND]

Commands:
  sync     Sync new photos from the configured backend album into local cache
  send     Send the next cached photo as a postcard (respects cooldown unless --force)
  daemon   Run automation daemon: periodic sync + scheduled postcard sending with cooldown
  quota    Show Swiss Post quota and local automation cooldown status
  albums   List available albums in the photo backend
  balance  Show PCC account balance
  user     Show PCC account information
  probe    Probe which app-version header/param the API accepts (one login)
  help     Print this message or the help of the given subcommand(s)

Options:
  -p, --plugin <PLUGIN>             Photo backend plugin to use: "immich" or "google-photos"
      --immich-url <URL>            Immich instance base URL [env: IMMICH_INSTANCE_URL]
      --immich-api-key <KEY>        Immich API key [env: IMMICH_API_KEY]
      --immich-album <ALBUM>        Immich album name or UUID [env: IMMICH_ALBUM]
      --media-folder <DIR>          Custom media cache folder [env: IMMICH_MEDIA_FOLDER]
      --insecure-tls                Allow self-signed or invalid SSL certificates
      --config <PATH>               Path to custom config JSON file
  -h, --help                        Print help
  -V, --version                     Print version
```

### Examples

#### Check remaining quota and cooldown
```sh
postcards-rust quota
```
*Output:*
```
=== Swiss Post Quota ===
  Quota:            -1
  Available Now:    Yes
  API Next Card:    Immediately
  Quota Valid End:  2027-09-10T21:14:57+00:00
  Retention Days:   7

=== Automation Cooldown Status ===
  Configured Interval:  7 days
  Total Cards Sent:     0
  Last Photo Sent:      None
  Last Card Sent At:    Never
  Cooldown Active:      NO (Ready to send)
  Next Eligible Send:   Ready now
```

#### List available albums on Immich
```sh
postcards-rust albums
```

#### Send the next postcard
Respects the 7-day cooldown. To send immediately:
```sh
postcards-rust send --force
```

#### Run the hands-off automation daemon
Checks for new photos, respects the 7-day cooldown and quota availability, sends the oldest card, unlinks it from the album, and sleeps until the next eligible window:
```sh
postcards-rust daemon --cooldown-days 7 --sync-minutes 60
```

---

## Docker Deployment

Build the container image:
```sh
docker build -t postcards-rust:latest .
```

Run with persistent storage for tokens and state:
```sh
docker run -d \
  --name postcards \
  --restart unless-stopped \
  -v $(pwd)/data:/data \
  -e IMMICH_INSTANCE_URL="https://photos.yourdomain.com" \
  -e IMMICH_API_KEY="your-api-key" \
  -e IMMICH_ALBUM="Postcards" \
  -e PCD_USERNAME="your-swissid-email" \
  -e PCD_PASSWORD="your-password" \
  postcards-rust:latest daemon
```

---

## Kubernetes Helm Chart

A complete Helm chart is provided in [`charts/postcards`](charts/postcards).

### 1. Create a `values.yaml` file
```yaml
mode: daemon

immich:
  instanceUrl: "https://photos.yourdomain.com"
  apiKey: "your-immich-api-key"
  album: "Postcards"

pcc:
  username: "your-swissid-email"
  password: "your-password"

# Optional: Seed cached token from desktop (~/.postcards_rust/token.json)
# to avoid entering SMS 2FA inside Kubernetes:
# token:
#   initialTokenJson: |
#     {"access_token":"...","refresh_token":"...","expires_in_seconds":300,"expires_at":"..."}

sender:
  firstName: "Max"
  lastName: "Muster"
  street: "Bahnhofstrasse 10"
  zip: "8001"
  city: "Zürich"

recipient:
  firstName: "Grandma"
  lastName: "Muster"
  street: "Musterstrasse 1"
  zip: "3000"
  city: "Bern"
  country: "SWITZERLAND"

persistence:
  enabled: true
  size: 1Gi
```

### 2. Install the Chart
```sh
helm install postcards ./charts/postcards -f values.yaml
```

### 3. Check logs & status
```sh
kubectl logs -f deployment/postcards
```

See [`charts/postcards/README.md`](charts/postcards/README.md) for full configuration parameters.

---

## SwissID 2FA Authentication & Token Setup

Swiss Post authentication is managed through SwissID, which enforces an SMS 2FA code verification on initial login.

### How Token Caching Works
1. During the initial login, PostcardsRust completes the SwissID OAuth handshake and prompts you for the 6-digit SMS verification code.
2. Upon entering the code, the resulting OAuth credentials (`access_token` and long-lived `refresh_token`) are automatically written to disk in `token.json` (`~/.postcards_rust/token.json` locally or `/data/.postcards_rust/token.json` in Docker/Kubernetes).
3. PostcardsRust automatically refreshes the short-lived access token using the cached refresh token prior to sending. **You only need to enter the 2FA SMS code once** as long as `token.json` is preserved across restarts on persistent storage.

---

### How to Provide the 2FA Code

#### Method 1: Interactive Docker Container (Recommended for Docker)
Run a temporary interactive container with `-it` attached to complete the initial login:

```sh
docker run -it --rm \
  -v $(pwd)/data:/data \
  -e PCD_USERNAME="your-swissid-email" \
  -e PCD_PASSWORD="your-swissid-password" \
  ghcr.io/cplusplus17/postcardsrust:latest quota
```

1. Enter your SMS code when prompted on the terminal.
2. The authenticated credentials will be saved to `$(pwd)/data/.postcards_rust/token.json`.
3. Start your background daemon using the same `-v $(pwd)/data:/data` volume mount. It will read the cached token and run hands-off without ever prompting for 2FA again.

---

#### Method 2: Headless Kubernetes (Zero-2FA Token Seeding)
If you run Kubernetes without interactive terminal access, seed the token from your desktop:

1. Run `postcards-rust quota` once on your local machine to generate `~/.postcards_rust/token.json`.
2. In your Helm `values.yaml`, paste the file's JSON contents into `token.initialTokenJson`:
   ```yaml
   token:
     initialTokenJson: |
       {"access_token":"...","refresh_token":"...","expires_in_seconds":300,"expires_at":"..."}
   ```
3. When the Helm chart is deployed, a Kubernetes init container automatically writes `token.json` into the PVC before the main daemon container boots.

---

#### Method 3: Live Kubernetes Terminal (`kubectl exec`)
If your pod is already running in the cluster and waiting for credentials:

```sh
# Run an interactive command inside the running pod
kubectl exec -it deployment/postcards -n postcards -- postcards-rust quota
```

Enter your SMS 2FA code in the interactive prompt. The token will be saved directly to the `/data/.postcards_rust/token.json` volume mount on the PVC, allowing the daemon to resume sending automatically.

---

## Image Scaling Pipeline

Swiss Post requires uploaded images to be formatted at **1819×1311 pixels**. The built-in image processor:
1. Detects portrait images (width < height) and rotates them 90°.
2. Scales the image preserving aspect ratio with **Lanczos3** filtering.
3. Center-crops to exactly 1819×1311.
4. Encodes as high-quality JPEG (quality 92) and base64-encodes for the upload payload.

---

## License

MIT License. See [`Cargo.toml`](Cargo.toml) for details.
