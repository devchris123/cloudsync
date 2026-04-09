# CLAUDE.md

## Project Overview

CloudSync is a self-hosted Dropbox alternative written in Rust. CLI-driven manual sync (push/pull/status) with content-addressable storage and BLAKE3 hashing.

## Workspace Structure

```
crates/
  cloudsync-common/   — Shared types (FileMeta, FileAction), protocol request/response types, BLAKE3 hashing utilities
  cloudsync-server/   — Axum HTTP REST API, redb metadata DB, content-addressable file storage, token auth middleware
  cloudsync-client/   — CLI tool (init/push/pull/status), sync engine, conflict detection, local state (redb)
docs/
  roadmap.md          — Next features: chunked uploads, easy setup, web UI
  plan-v0.1.md        — Original implementation plan (completed)
  server-setup.md     — Hetzner VPS deployment guide
```

## Commit Style

- **Subject**: concise imperative one-liner (~50 chars), capitalized, no trailing period
- **Body**: explain **why** the change was made, not what changed — the diff shows that. Keep it brief (2-4 sentences). Focus on motivation, trade-offs, and what it enables.
- Commits should be small and self-contained; each must leave the code in a working state
- If changes span unrelated concerns, split into multiple commits

Good body example:
> Chunked upload speeds up uploads for large files (>4MB up to ~GB)
> by sending file bytes in chunks. The server reassembles on finalize.
> Does not block implementing retry on top of it.

Bad body example (just repeating the diff):
> - Implement CRUD in upload_db for the new upload table
> - Implement chunk endpoints for parallel upload
> - Implement client logic for chunked upload

## Build & Test

```bash
cargo build                 # Build all crates
cargo test                  # Run all tests
cargo clippy -- --deny warnings  # Lint (CI enforces this)
cargo fmt --check           # Format check (CI enforces this)
```

Rust toolchain pinned to **1.91.1** via `rust-toolchain.toml`.

## CI

PR checks (`.github/workflows/ci.yml`): rustfmt, clippy with `--deny warnings`, tests. Docs/markdown changes skip CI.

Release (`.github/workflows/release.yml`): on `v*` tags — GitHub release, Docker image to GHCR, deploy to Hetzner.

## Key Patterns

- **Content-addressable storage**: files stored at `{storage_dir}/{hash[0..2]}/{hash}`
- **redb**: embedded key-value DB for metadata (server) and sync state (client)
- **SyncApi trait**: abstraction over HTTP client, mock-friendly for testing
- **Soft deletes**: `is_deleted` flag on FileMeta, not hard removal
- **Monotonic versioning**: per-file version counter (avoids clock-skew issues)
- **Streaming BLAKE3**: 4MB buffer reads for memory-efficient hashing

## Server Configuration

Environment variables / CLI flags:
- `CLOUDSYNC_HOST` (default: 127.0.0.1)
- `CLOUDSYNC_PORT` (default: 3050)
- `CLOUDSYNC_TOKEN` (required)
- `CLOUDSYNC_STORAGE_DIR` (default: cloudsync/data/files)
- `CLOUDSYNC_DBNAME` (default: data.redb)

## API Endpoints

All require `Authorization: Bearer <token>` except health:
- `GET /api/v1/health` — health check
- `GET /api/v1/files` — list all files
- `GET /api/v1/files/{path}` — download file
- `POST /api/v1/files` — upload file (multipart: path + file)
- `DELETE /api/v1/files/{path}` — soft-delete file

## Current State

v0.1 complete. Branch `devchris123/roadmap` has partial scaffolding for chunked uploads (types + DB layer, not yet wired to endpoints).
