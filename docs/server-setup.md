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
- Create the data directory:

```sh
mkdir -p /mnt/volume-cloudsync/cloudsync
```

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

## 6. First Deploy

The release workflow (`.github/workflows/release.yml`) handles:

- Building and pushing the Docker image to ghcr.io
- SSHing into the server
- Copying `docker-compose.yml` to the server
- Running `docker compose up -d` with env vars for image tag, mount dir, and token

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
