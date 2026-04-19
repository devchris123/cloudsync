# CloudSync Roadmap

**Vision: "The private Dropbox you host yourself."**

Privacy-first, E2E encrypted, easy to self-host. Start as a single-user self-hosted tool, grow into a lightweight managed service for privacy-conscious users.

---

## Carried over from v0.1

See GitHub issues for details:

- #13 — Auto-generate auth token on first run
- #14 — Improved health endpoint
- #15 — Web UI: upload and delete
- #16 — API: directory browsing and file metadata

---

## Phase 1: Network-ready

### HTTPS via reverse proxy
- Server listens on HTTP on localhost only; TLS handled by a reverse proxy (bring your own)
- Ship a `Caddyfile` + Caddy service in `docker-compose.yml` as the default (automatic Let's Encrypt, zero config)
- Document nginx/Traefik alternatives for users who already have a reverse proxy

### Resumable uploads
- Client checks `GET /api/v1/uploads/{upload_id}` on start to find already-received chunks
- Skip completed chunks, upload only what's missing
- Handle expired/cleaned-up uploads gracefully (restart from scratch)
- Parallel chunk uploads (e.g. `tokio::JoinSet`) for faster large-file transfers
- Parallel small-file uploads to reduce latency overhead when syncing many files below chunk threshold

### Resumable downloads
- Server: support HTTP `Range` requests on `GET /api/v1/files/{path}`
- Client: on interrupted download, resume from last byte received
- Verify final file hash after reassembly

---

## Phase 1.5: Storage hygiene

### Server-side garbage collection
- Purge incomplete/expired uploads (orphaned chunks from abandoned uploads)
- Permanently remove soft-deleted files (`is_deleted`) after a configurable retention period
- Reclaim content-addressable blobs no longer referenced by any file metadata

---

## Phase 2: Encryption

### Client-side E2E encryption
- Files encrypted on the client before upload, decrypted after download
- Server never sees plaintext — zero-knowledge design
- Encryption key derived from a user passphrase (Argon2 KDF)
- Per-file random nonce, symmetric encryption (XChaCha20-Poly1305 or AES-256-GCM)
- Encrypted file metadata (filenames, sizes) stored alongside encrypted content
- Key management: master key encrypted with passphrase, stored locally; recovery flow TBD

### Encrypted chunked uploads
- Each chunk encrypted individually before upload
- Server stores opaque encrypted blobs — existing CAS model unchanged
- Client reassembles and decrypts on download

---

## Phase 3: Client experience

### Desktop app (Tauri)
- System tray app with background sync daemon
- Watch a local folder, auto-sync changes (push on file change, periodic pull)
- Reuse existing Rust client crate as the sync engine
- Minimal UI: sync status, recent activity, settings (server URL, token, sync folder)
- Graceful upload cancellation (cancellation token) — cancel button needs to stop uploads without killing the app

### Conflict resolution UX
- Surface conflicts in the desktop app and web UI
- Let users pick which version to keep or keep both

---

## Phase 4: Multi-user / SaaS prep

### User accounts
- Replace single shared token with per-user accounts (username + password or API key)
- Isolated storage namespaces per user
- Server-side auth: sessions or JWT

### Admin dashboard
- Build on the improved health endpoint
- User management, storage quotas, usage stats

### Multi-tenant deployment
- Single server instance serving multiple users
- Per-user storage limits and access control
- Foundation for a managed/hosted offering
