# PostcardsRust

Rust implementation of [PostcardsDotnet](https://github.com/CPlusPlus17/PostcardsDotnet)
(a .NET port of [abertschi/postcards](https://github.com/abertschi/postcards)).

Syncs photos from a Google Photos album and automatically sends them as
digital postcards through the [Swiss Post Postcard Creator](https://postcardcreator.post.ch).

## Why a Rust fork

- Single static binary (release: ~10 MB) instead of a .NET runtime
- `async`/`tokio` for the login + API flows
- Same environment variables as the .NET CLI, so existing setups keep working
- Workspace layout mirrors the .NET solution (core / api / plugin-base / google-photos / cli)

## Crates

| crate | .NET counterpart | purpose |
|---|---|---|
| `postcards-rust-core` | `PostcardsDotnet.Common` + `.Services` + `.Contracts` + `.Data.*` | SwissId login, PCC REST API, image scaling |
| `postcards-rust-api` | `PostcardsDotnet.API` | `SwissPostcardCreatorApi` facade (login, send, quota, balance, user) |
| `postcards-rust-plugin-base` | `PostcardsDotnet.PluginBase` | `ICommand` plugin trait, `AlbumSummary` |
| `postcards-rust-plugin-immich` | New | Immich album sync & automatic post-send unlinking |
| `postcards-rust-plugin-google-photos` | `PostcardsDotnet.PluginGooglePhotos` | Google Photos album sync |
| `postcards-rust-cli` | `PostcardsDotnet.Cli` | CLI + automation daemon + cooldown tracking |

## Build

```sh
cargo build --release
cargo test --workspace
```

## Photo Backends (Plugins)

### 1. Immich (Recommended)

Connects to your self-hosted Immich instance, synchronizes photos from a chosen album (e.g. "Postcards"), sends the oldest photo, and **automatically removes the sent photo from the album** (without deleting it from your library).

Configure via CLI flags, environment variables, or `~/.postcards_rust/config.json`:

```sh
# Immich Environment Variables
export IMMICH_INSTANCE_URL="https://photos.example.com"
export IMMICH_API_KEY="your-api-key-here"
export IMMICH_ALBUM="Postcards"  # album name or UUID
# export IMMICH_INSECURE_TLS=true  # optional, for self-signed certificates
```

Or JSON config file (`~/.postcards_rust/config.json`):

```json
{
  "plugin": "immich",
  "cooldown_days": 7,
  "default_message": "Sent automatically from Immich!",
  "immich": {
    "instance_url": "https://photos.example.com",
    "api_key": "your-api-key-here",
    "album": "Postcards"
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
    "city": "Bern"
  }
}
```

### 2. Google Photos

Reads the same env vars as the legacy .NET implementation:

```sh
export GPSC_USER=you@example.com
export GPSC_CLIENTID=<google client id>
export GPSC_CLIENTSECRET=<google client secret>
export GPSC_MEDIAFOLDERPATH=/var/lib/postcards/media
export GPSC_ALBUMSTOSYNC="My Album,Other Album"
export GPSC_SYNCEDIDSFILEPATH=/var/lib/postcards/synced_ids.txt
export GPSC_CONFIGPATH=/var/lib/postcards/google_token.json
```

## PCC Configuration (Swiss Post)

Cached tokens in `~/.postcards_rust/token.json` are reused automatically across runs. To log in initially:

```sh
export PCD_USERNAME=<swissid login>
export PCD_PASSWORD=<password>
```

## CLI Subcommands

```
postcards-rust sync     # Sync new photos from the configured backend album into local cache
postcards-rust send     # Send the next photo as a postcard (respects cooldown, --force to bypass)
postcards-rust daemon   # Run automation daemon: periodic sync + scheduled sending with cooldown
postcards-rust quota    # Show Swiss Post quota and automation cooldown status
postcards-rust albums   # List all available albums on your Immich instance
postcards-rust balance  # Show PCC account balance
postcards-rust user     # Show PCC account information
```

### Automation & 7-Day Cooldown

- Swiss Post free cards have a cooldown retention period (typically 7 days).
- PostcardsRust enforces this cooldown in `send` and automatically schedules the next send in `daemon`.
- State is persisted in `~/.postcards_rust/state.json`.
- Override at any time with `postcards-rust send --force`.

### One-time Google Photos login

If `GPSC_CONFIGPATH` does not contain a token yet, `login` prints an
authorization URL. Open it in a browser, grant access, and paste the code
back. The refresh token is stored in `GPSC_CONFIGPATH` and reused on
subsequent runs (no browser needed afterwards).

## How the login works (SwissId)

Ported 1:1 from the .NET `SwissIdLoginService`:

1. `pccweb.api.post.ch/OAuth/authorization` — seed cookies (PKCE `code_challenge`)
2. `account.post.ch/idp/?login` — extract the `goto` parameter
3. `login.swissid.ch/api-login/...` — `token/status`, `welcome-pack`, `authenticate/init` (authId), `authenticate/basic`
4. poll `authenticate/swiss-id-app/status` while 2FA (SwissId app push) is pending
5. `anomaly-detection/device-print` — get the next URL
6. follow the URL, extract `SAMLResponse` + `RelayState`
7. `pccweb.api.post.ch/OAuth/` (code) → `/OAuth/token` (access + refresh token)

Tokens are refreshed automatically before expiry when sending.

## Image pipeline

The Postcard Creator requires 1819×1311. `image_helper` reproduces the
ImageMagick pipeline of the .NET version:

- portrait images are rotated 90°
- scale to fill 1819×1311 (aspect preserved)
- center-crop to exactly 1819×1311
- encode JPEG (quality 92) → base64

## License

MIT (see `Cargo.toml`).
