# Server Setup

Manual steps to provision a new CloudSync server (Hetzner VPS, Ubuntu 24.04).

## 1. Create VPS

- Provider: Hetzner (CX22 or similar)
- Image: Ubuntu 24.04 LTS with Docker CE pre-installed
- Add SSH public key during creation
- Attach a volume for persistent storage

## 2. SSH Access

```sh
ssh root@<server-ip>
```

### Disable password authentication

```sh
sudo sed -i 's/^#\?PasswordAuthentication.*/PasswordAuthentication no/' /etc/ssh/sshd_config
sudo systemctl restart ssh
```

Verify: `ssh -o PubkeyAuthentication=no root@<server-ip>` should return `Permission denied (publickey)`.

## 3. Volume Setup

- Mount the Hetzner volume to `/mnt/volume-cloudsync`
- Remove any duplicate mount points from `/etc/fstab`
- Create one subdirectory per service that needs persistent state:

```sh
mkdir -p /mnt/volume-cloudsync/{cloudsync,caddy,keycloak}
```

| Directory  | Maps to env var               | Backs                                  |
| ---------- | ----------------------------- | -------------------------------------- |
| `cloudsync/` | `CLOUDSYNC_MOUNT_DIR`         | cloudsync server: file storage + redb  |
| `caddy/`     | `CLOUDSYNC_CADDY_MOUNT_DIR`   | caddy: Let's Encrypt certs, state      |
| `keycloak/`  | `CLOUDSYNC_KC_DATA_DIR`       | keycloak: H2 database, signing keys    |

### Permissions

Each container's process needs write access to its bind-mounted directory.
Containers running as root inside the image (cloudsync, caddy) can write to
`root:root 755` dirs, so they need no chown. Keycloak's official image runs
as uid 1000 and will fail to start if it can't write to its data dir:

```sh
chown 1000:1000 /mnt/volume-cloudsync/keycloak
```

If we ever harden the cloudsync or caddy images to run as a non-root user
(see roadmap), the same chown becomes required for those dirs too — pick a
uid, declare it in the Dockerfile (or `user:` in compose), and `chown` the
matching host dir.

## 4. Deploy Key for CI/CD

Generate a deploy key pair locally:

```sh
ssh-keygen -t ed25519 -C "cloudsync-deploy" -f /tmp/deploy_key -N ""
```

Copy public key to the server:

```sh
ssh root@<server-ip> "echo '$(cat /tmp/deploy_key.pub)' >> ~/.ssh/authorized_keys"
```

Add private key as a GitHub secret:

```sh
gh secret set DEPLOY_SSH_KEY < /tmp/deploy_key
```

Delete local key files:

```sh
rm /tmp/deploy_key /tmp/deploy_key.pub
```

## 5. Environment

Set the `CLOUDSYNC_TOKEN` on the server (or store as a GitHub secret and pass during deploy).

For the observability stack (see §7), also set `CLOUDSYNC_GRAFANA_ADMIN_PASSWORD` as a GitHub secret:

```sh
gh secret set CLOUDSYNC_GRAFANA_ADMIN_PASSWORD
```

### GitHub secrets used by `deploy.yml`

| Secret                             | What it's for                                      |
| ---------------------------------- | -------------------------------------------------- |
| `CLOUDSYNC_HETZNER_PRIVATE_KEY`    | SSH key for the deploy user on the VPS             |
| `CLOUDSYNC_TOKEN`                  | Static bearer token for the API (fallback auth)    |
| `CLOUDSYNC_GRAFANA_ADMIN_PASSWORD` | Grafana admin password                             |
| `CLOUDSYNC_KC_ADMIN`               | Keycloak bootstrap admin username                  |
| `CLOUDSYNC_KC_ADMIN_PASSWORD`      | Keycloak bootstrap admin password (rotate after 1st login) |

## 6. Release and Deploy

Two separate steps: tag a release to publish artifacts, then trigger a
deploy to roll it out. Splitting them lets you publish without deploying,
deploy any past tag (rollback), or re-deploy the same tag without re-tagging.

### Release

```sh
git checkout main && git pull
git tag -a v0.3.0 -m "Short summary of what's in this release"
git push origin v0.3.0
```

The release workflow (`release.yml`) triggers on the tag push and:

- Builds client binaries for linux x86_64/aarch64 and macOS intel/arm
- Builds the server binary for linux x86_64
- Creates a GitHub release with auto-generated notes
- Pushes the server Docker image to `ghcr.io/devchris123/cloudsync-server`
  with tags `:<version>` and `:latest`

Takes ~10 minutes for all targets. Watch progress in the Actions tab.

### Deploy

After the release workflow finishes, trigger a deploy:

```sh
gh workflow run deploy.yml -f version=v0.3.0
```

Or via the GitHub UI: Actions → Deploy → Run workflow → enter the version.

The deploy workflow (`deploy.yml`) SSHes to the VPS, copies the current
repo's `docker-compose.yml` and config dirs (caddy, observability), and
runs `docker compose up -d` with env vars pointing at the chosen image
tag. The compose file shipped is the one at `HEAD` of `main` at deploy
time, not the one at the tagged commit — keep that in mind if you ever
need to deploy an old version against a new compose layout.

## 7. Observability (Grafana + Loki + Promtail)

Logs from every container on the host flow through Promtail into Loki and are
browseable in Grafana at `https://monitoring.devchris.dev`.

One-time DNS setup: add an A record for `monitoring.devchris.dev`
pointing to the VPS IP. Caddy will provision the TLS cert automatically on
first request.

The deploy workflow creates `/mnt/volume-cloudsync/loki` and
`/mnt/volume-cloudsync/grafana` with the right ownership (UID 10001 and 472
respectively) — no manual setup needed beyond the DNS record and the
`CLOUDSYNC_GRAFANA_ADMIN_PASSWORD` secret.

First login: username `admin`, password from the secret. Loki is
auto-provisioned as the default datasource; use the **Explore** tab and query
`{container="cloudsync"}` to tail server logs.

## 8. Keycloak realm operations

### Realm bootstrap is one-shot

`docker-compose.yml` mounts `keycloak/realm-cloudsync.json` and runs Keycloak
with `start --import-realm`. That flag means **create the realm on first
boot, ignore the file on every subsequent boot.** The data lives in
Keycloak's database under `${CLOUDSYNC_KC_DATA_DIR}`; that volume is the
source of truth from that point on.

Practical consequence: editing the JSON and re-deploying does nothing. The
realm in the running Keycloak does not change. Edits to redirect URIs,
client attributes, role mappings, etc. have to be applied via the admin
console at `https://auth.devchris.dev` — or via the reconciling import
described below.

### Reconciling JSON changes into a running Keycloak

For non-destructive updates (new redirect URIs, scope changes, mapper
tweaks), use `kc.sh import` with `--override true` against the running
container:

```sh
docker compose exec keycloak \
  /opt/keycloak/bin/kc.sh import \
  --file /opt/keycloak/data/import/realm-cloudsync.json \
  --override true
```

This **reconciles** declared objects: anything in the JSON gets updated to
match. Two important caveats:

1. **It is additive, not mirroring.** Removing a redirect URI from the JSON
   does NOT remove it from the running client. To delete, edit via admin
   console.
2. **It overwrites declared objects wholesale.** If a human edited a client
   secret or mapper config through the UI, that edit is lost when you
   re-import.

For richer GitOps semantics (delete-on-removal, dry-run diffs) the
community option is [`keycloak-config-cli`](https://github.com/adorsys/keycloak-config-cli).
Not needed at current scale.

### First-deploy checklist

After Keycloak boots and the realm is imported the first time, verify in
the admin console:

- **Clients → cloudsync → Valid redirect URIs** contains the prod URLs (web
  callback + post-logout target) and `http://127.0.0.1/callback` for the
  CLI loopback flow. Localhost dev URIs should NOT be in prod.
- **Clients → cloudsync → Advanced → OAuth 2.0 Device Authorization Grant
  Enabled** is ON. (The JSON sets it; if the import predates this PR, flip
  it by hand.)
- **Clients → cloudsync → Settings → Consent required** is ON.
- **Realm settings → Sessions** — review default token lifespans before
  going live.

### `iss` claim alignment

The JWT `iss` claim must match `CLOUDSYNC_OIDC_ISSUER` exactly. Verify with:

```sh
curl -s https://auth.devchris.dev/realms/cloudsync/.well-known/openid-configuration \
  | jq -r .issuer
```

If it doesn't say `https://auth.devchris.dev/realms/cloudsync`, fix
`KC_HOSTNAME` in the compose file rather than the env var — Keycloak's
hostname config is the source of the `iss` claim.

## 9. Cookie posture: pin the public base URL

`docker-compose.yml` sets `CLOUDSYNC_PUBLIC_BASE_URL=https://cloudsync.devchris.dev`.
**Keep it set.**

The server decides whether to mint cookies with the `Secure` flag by
parsing the public base URL. Without `CLOUDSYNC_PUBLIC_BASE_URL` set, that
URL is derived from the request's `X-Forwarded-Proto` and `X-Forwarded-Host`
headers. A misconfigured proxy that drops `X-Forwarded-Proto: https`
silently downgrades session cookies to non-`Secure` — they then travel in
cleartext to any plain-HTTP client, and browsers send them on plain-HTTP
links to the same host. The login flow keeps working; the downgrade is
invisible.

Pinning the env var makes the decision deterministic. The startup log
emits a warning if it's unset on a non-loopback bind — watch for it in
Grafana the first time you deploy a new host.
