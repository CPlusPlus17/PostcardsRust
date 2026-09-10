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
| `postcards-rust-plugin-base` | `PostcardsDotnet.PluginBase` | `ICommand` plugin trait |
| `postcards-rust-plugin-google-photos` | `PostcardsDotnet.PluginGooglePhotos` | Google Photos album sync |
| `postcards-rust-cli` | `PostcardsDotnet.Cli` | CLI + daemon loop |

## Build

```sh
cargo build --release
cargo test --workspace
```

## Usage

The CLI reads the same env vars as the .NET implementation:

```sh
# Google Photos (required for the google-photos plugin)
export GPSC_USER=you@example.com
export GPSC_CLIENTID=<google client id>
export GPSC_CLIENTSECRET=<google client secret>
export GPSC_MEDIAFOLDERPATH=/var/lib/postcards/media
export GPSC_ALBUMSTOSYNC="My Album,Other Album"
export GPSC_SYNCEDIDSFILEPATH=/var/lib/postcards/synced_ids.txt
export GPSC_CONFIGPATH=/var/lib/postcards/google_token.json

# PCC (required for sending)
export PCD_USERNAME=<swissid login>
export PCD_PASSWORD=<password>
export PCD_SENDERFIRSTNAME=...
export PCD_SENDERLASTNAME=...
export PCD_SENDERSTREET=...
export PCD_SENDERZIP=...
export PCD_SENDERCITY=...
export PCD_RECIPIENTFIRSTNAME=...
export PCD_RECIPIENTLASTNAME=...
export PCD_RECIPIENTSTREET=...
export PCD_RECIPIENTZIP=...
export PCD_RECIPIENTCITY=...

# daemon sync period in minutes (default 600)
export PCDNCLI_PLUGINSYNCTIME=600
```

Subcommands:

```
postcards-rust sync     # sync new photos from the plugin into the local cache
postcards-rust send     # send the next cached photo as a postcard
postcards-rust quota    # show the PCC quota
postcards-rust balance  # show the PCC account balance
postcards-rust user     # show PCC account information
postcards-rust daemon   # run forever: periodic sync + daily send (default)
```

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
