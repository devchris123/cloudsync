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
