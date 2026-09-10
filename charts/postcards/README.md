# PostcardsRust Helm Chart

A Kubernetes Helm chart for **PostcardsRust** — automated Swiss Postcard Creator with self-hosted **Immich** (or Google Photos) photo backend.

## Features

- **Automated Sending**: Synchronizes photos from an Immich album, scales them to Swiss Post specifications, uploads postcards, and unlinks sent photos from the album automatically.
- **7-Day Cooldown Awareness**: Enforces the Swiss Post 7-day retention period.
- **Two Operating Modes**:
  - `mode: daemon` (default): Single-replica background Deployment running continuous sync and 7-day cooldown scheduling.
  - `mode: cronjob`: Kubernetes native `CronJob` running scheduled sends (e.g. weekly).
- **Persistent State**: PersistentVolumeClaim (PVC) stores cached tokens, 7-day cooldown state, and album cache across Pod restarts and upgrades.
- **Zero-2FA Headless Deployment**: Optional `token.initialTokenJson` seeds your existing local `~/.postcards_rust/token.json` directly onto the PVC on first boot, completely bypassing SMS 2FA in headless Kubernetes!

## Prerequisites

- Kubernetes cluster 1.24+
- Helm 3.8+
- A ReadWriteOnce PersistentVolume provider (e.g. local-path, EBS, Longhorn, Ceph, etc.)

## Quick Start

### 1. Create a custom values file (`postcards-values.yaml`)

```yaml
mode: daemon

immich:
  instanceUrl: "https://immich.yourdomain.com"
  apiKey: "YOUR_IMMICH_API_KEY"
  album: "Postcards"

pcc:
  username: "your-email@example.com"
  password: "your-password"

# Optional: Seed cached token from your desktop (~/.postcards_rust/token.json)
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
```

### 2. Install the Chart

```sh
helm install postcards ./charts/postcards -f postcards-values.yaml
```

### 3. Check Status

```sh
# View pod logs
kubectl logs -f deployment/postcards

# Or execute a quota check inside the container
kubectl exec -it deployment/postcards -- postcards-rust quota
```

## Configuration Reference

| Parameter | Description | Default |
|---|---|---|
| `mode` | `daemon` (Deployment) or `cronjob` (CronJob) | `daemon` |
| `daemon.cooldownDays` | Cooldown period in days between sent cards | `7` |
| `daemon.syncMinutes` | Interval in minutes to sync photos from album | `60` |
| `daemon.forceInitial` | Attempt immediate send on pod boot | `false` |
| `cronjob.schedule` | Cron schedule expression (when `mode: cronjob`) | `"0 8 * * 0"` (Weekly Sunday) |
| `plugin` | Photo backend plugin (`immich` or `google-photos`) | `immich` |
| `immich.instanceUrl` | Immich instance base URL | `""` |
| `immich.apiKey` | Immich API key | `""` |
| `immich.album` | Album name or UUID | `"Postcards"` |
| `immich.insecureTls` | Allow self-signed TLS certificates | `false` |
| `immich.existingSecret` | Existing Secret name for Immich API key | `""` |
| `pcc.username` | SwissID login email | `""` |
| `pcc.password` | SwissID password | `""` |
| `pcc.existingSecret` | Existing Secret name for SwissID credentials | `""` |
| `token.initialTokenJson` | Seed token JSON string to bypass 2FA | `""` |
| `persistence.enabled` | Enable PVC for tokens and state persistence | `true` |
| `persistence.size` | PVC storage size | `1Gi` |
| `resources` | Container resource requests & limits | `50m/64Mi` - `500m/512Mi` |

## Upgrading & Uninstalling

To upgrade the release:

```sh
helm upgrade postcards ./charts/postcards -f postcards-values.yaml
```

To uninstall:

```sh
helm uninstall postcards
```
*(Note: PVC will be preserved or deleted based on your cluster's reclaim policy)*
