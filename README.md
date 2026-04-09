# CloudSync

A self-hosted Dropbox alternative written in Rust. CLI-driven manual sync with content-addressable storage and BLAKE3 hashing.

## Features

- **Push/Pull/Status** workflow (no background sync daemon)
- **Content-addressable storage** with BLAKE3 hashing
- **Chunked uploads** for large files (4MB chunks, hash-verified reassembly)
- **Conflict detection** with automatic conflict file creation
- **Soft deletes** with monotonic versioning (no clock-skew issues)
- **Token-based auth** over HTTP
- **Single-binary server** with embedded redb database
- **Docker deployment** with CI/CD via GitHub Actions

## Quickstart

### Server

Create a `.env` file:

```
CLOUDSYNC_TOKEN=your-secret-token
CLOUDSYNC_MOUNT_DIR=/path/to/data
```

Then run:

```sh
docker compose up -d
```

### Client

```sh
# Initialize a sync directory
cloudsync init --server-url http://your-server:3050 --token your-secret-token

# Push local changes to server
cloudsync push

# Pull server changes to local
cloudsync pull

# Check sync status
cloudsync status
```

## Building from source

Requires Rust 1.91.1 (pinned via `rust-toolchain.toml`).

```sh
cargo build --release
```

Binaries:
- `target/release/cloudsync` — CLI client
- `target/release/cloudsync-server` — HTTP server

## Running tests

```sh
cargo test
```

## Project structure

```
crates/
  cloudsync-common/   — Shared types, protocol messages, BLAKE3 hashing
  cloudsync-server/   — Axum HTTP API, redb metadata DB, content-addressable storage
  cloudsync-client/   — CLI tool, sync engine, conflict detection, local state
docs/
  roadmap.md          — Planned features
  server-setup.md     — Hetzner VPS deployment guide
```

## Server configuration

| Variable | Default | Description |
|---|---|---|
| `CLOUDSYNC_HOST` | `127.0.0.1` | Bind address |
| `CLOUDSYNC_PORT` | `3050` | Bind port |
| `CLOUDSYNC_TOKEN` | *(required)* | Auth token |
| `CLOUDSYNC_STORAGE_DIR` | `cloudsync/data/files` | Content-addressable file storage |
| `CLOUDSYNC_STAGING_DIR` | `cloudsync/data/staging` | Temporary chunked upload staging |
| `CLOUDSYNC_DBNAME` | `data.redb` | Metadata database path |

## API

All endpoints require `Authorization: Bearer <token>` except health.

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/health` | Health check |
| `GET` | `/api/v1/files` | List all files |
| `GET` | `/api/v1/files/{path}` | Download file |
| `POST` | `/api/v1/files` | Upload file (multipart) |
| `DELETE` | `/api/v1/files/{path}` | Soft-delete file |
| `POST` | `/api/v1/uploads` | Init chunked upload |
| `PUT` | `/api/v1/uploads/{id}/chunks/{index}` | Upload chunk |
| `GET` | `/api/v1/uploads/{id}` | Upload status |
| `POST` | `/api/v1/uploads/{id}/finalize` | Finalize chunked upload |

## License

MIT
