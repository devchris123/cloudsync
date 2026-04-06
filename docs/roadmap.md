# CloudSync Next Features Roadmap

## Context

CloudSync is a working self-hosted Dropbox alternative with CLI push/pull/status, content-addressable storage, conflict detection, Docker deployment, and CI/CD. The next features aim to differentiate it commercially:

1. **Large file handling** — current 50MB limit, entire files loaded into memory
2. **Easy setup** — currently 6+ manual steps to deploy
3. **Simple web UI** — currently CLI only

These are listed in dependency order: large files enables UI upload progress, easy setup makes testing/demo easier.

---

## Phase 1: Large File Handling (Chunked, Parallel, Resumable)

### 1.1 Streaming hash in `cloudsync-common`
- Add `hash_file_streaming(path: &Path) -> Result<String>` using incremental BLAKE3 hasher with 4MB read buffers instead of `std::fs::read` into memory
- Add `pub const CHUNK_SIZE: usize = 8 * 1024 * 1024;` (8MB default)
- File: `crates/cloudsync-common/src/lib.rs`

### 1.2 Chunked upload protocol types in `cloudsync-common`
- `InitUploadRequest { path, total_size, total_hash, chunk_count }`
- `InitUploadResponse { upload_id }` (UUID)
- `UploadChunkResponse { chunk_index }`
- `FinalizeUploadRequest { upload_id }`
- `FinalizeUploadResponse { file: FileMeta }`
- `UploadStatusResponse { upload_id, chunks_received: Vec<u32>, total_chunks }`
- File: `crates/cloudsync-common/src/lib.rs`

### 1.3 Server chunked upload endpoints
- `POST /api/v1/uploads` — init upload, create staging dir, track in redb
- `PUT /api/v1/uploads/{upload_id}/chunks/{index}` — receive chunk as raw body, verify per-chunk hash
- `GET /api/v1/uploads/{upload_id}` — return upload status (which chunks received)
- `POST /api/v1/uploads/{upload_id}/finalize` — assemble chunks, verify total hash, move to CAS, clean up staging
- Per-route body limits: chunk route gets `CHUNK_SIZE + overhead`, legacy upload keeps 50MB
- New redb table `UPLOADS_TABLE` for in-progress uploads with `created_at` for cleanup
- Staging dir: sibling to `storage_dir` — e.g. if `CLOUDSYNC_STORAGE_DIR=/data/files`, staging goes to `/data/uploads/{upload_id}/{chunk_index}`
- Background task: clean up abandoned uploads older than 24h
- Files: `crates/cloudsync-server/src/app.rs`, `crates/cloudsync-server/src/storage.rs`, `crates/cloudsync-server/src/db.rs`

### 1.4 Client chunked upload
- In `push_single_file`: if file > 10MB, use chunked path; otherwise use existing single-request path
- `push_single_file_chunked`: compute streaming hash, init upload, check status (for resume), upload chunks in parallel (tokio::JoinSet, concurrency 4), finalize
- Extend `SyncApi` trait with: `init_upload`, `upload_chunk`, `get_upload_status`, `finalize_upload`
- New methods on `SyncClient` in `client.rs`
- Files: `crates/cloudsync-client/src/sync.rs`, `crates/cloudsync-client/src/client.rs`

### 1.5 Streaming download
- Server: return `axum::body::Body` wrapping `tokio::fs::File` via `ReaderStream` instead of `Vec<u8>`, add `Content-Length` header
- Client: stream response to disk via `response.bytes_stream()` + `tokio::io::copy` instead of collecting to `Vec<u8>`
- New workspace deps: `tokio-util = { version = "0.7", features = ["io"] }`
- Files: `crates/cloudsync-server/src/app.rs`, `crates/cloudsync-client/src/client.rs`, `Cargo.toml`

### Key decisions
- **Chunk size: 8MB** — 1GB file = ~128 chunks, good balance of throughput vs retry granularity
- **Chunk storage**: temporary staging dir, assembled on finalize into CAS (preserves content-addressable model)
- **Separate trait**: `ChunkedUploadApi` trait alongside existing `SyncApi` to avoid bloating the mock

---

## Phase 2: Easy Setup

### 2.1 Auto-generate auth token on first run
- Make `--token` / `CLOUDSYNC_TOKEN` optional
- If no token provided: generate random 256-bit hex token, write to `{storage_dir}/.token`, print to stdout
- On subsequent starts: read from `.token` file if env var not set
- Add `rand` dependency
- Files: `crates/cloudsync-server/src/cli.rs`, `crates/cloudsync-server/src/main.rs`

### 2.2 docker-compose.yml
- Single service, GHCR image, port 3050, named volume to `/data`
- No token needed — auto-generates on first run, visible in `docker compose logs`
- New file: `docker-compose.yml`

### 2.3 Improved health endpoint
- Return version, file count, storage bytes, uptime
- Foundation for future dashboard
- Files: `crates/cloudsync-server/src/app.rs`, `crates/cloudsync-common/src/lib.rs`

### 2.4 Cross-compilation in CI
- Build matrix: `x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, `aarch64-darwin`
- Attach all binaries to GitHub Release
- File: `.github/workflows/release.yml`

### 2.5 Install script
- `curl -fsSL .../install.sh | sh` — detects OS/arch, downloads correct binary
- Depends on cross-compilation (2.4)
- New file: `install.sh`

---

## Phase 3: Simple Web UI

### 3.1 Approach: React + TypeScript SPA with `rust-embed`
- React + TypeScript — user has experience, good AI code generation support
- Keep it minimal: no Redux, no complex routing, just a few components
- Build with Vite into static files, embed into server binary via `rust-embed`
- Preserves single-binary deployment
- Adds a Node build step to CI (build UI before `cargo build`)
- Auth: login page accepts token, stores in sessionStorage, sends as Bearer
- New directory: `ui/` at workspace root (Vite + React + TypeScript project)

### 3.2 UI features
- File browser: table with name, size, modified date. Click to download. Breadcrumb nav for directories.
- Upload: drag-and-drop + file picker. Large files use chunked upload API with progress bar.
- Delete: per-file button with confirmation
- Build output goes to `crates/cloudsync-server/ui-dist/` for embedding

### 3.3 Embed and serve static files
- Add `rust-embed = "8"` to server deps
- `#[derive(RustEmbed)] #[folder = "ui-dist/"] struct UiAssets;`
- Routes: `GET /` serves `index.html`, `GET /assets/{*path}` serves embedded files
- UI routes are public (no auth), API routes still require Bearer token
- File: `crates/cloudsync-server/src/app.rs`
- Dockerfile: add Node stage to build UI before Rust build

### 3.4 API adjustments
- Add `GET /api/v1/files/{path}/meta` for file metadata without downloading content
- Add `?prefix=dir/` query param to `list_files` for directory-style browsing
- File: `crates/cloudsync-server/src/app.rs`

---

## Build Order (recommended for solo developer)

1. Phase 1.1 + 1.2 — streaming hash + protocol types (foundation)
2. Phase 1.3 + 1.5 — server streaming upload + download
3. Phase 1.4 — client chunked upload + streaming download
4. Phase 2.1 + 2.3 — auto-token + health endpoint (quick wins)
5. Phase 2.2 — docker-compose
6. Phase 3.3 + 3.4 — embed static files + API adjustments (plumbing)
7. Phase 3.2 — build the actual UI
8. Phase 2.4 + 2.5 — cross-compilation + install script (polish)

---

## Verification

- **Phase 1**: Push/pull a 1GB+ file, verify memory stays flat (no OOM), verify resume by killing mid-upload and restarting, verify parallel chunks via server logs
- **Phase 2**: `docker compose up` on a fresh machine, verify token auto-generated in logs, `cloudsync init` with the token, push/pull files
- **Phase 3**: Open browser to server URL, log in with token, browse files, upload a file via drag-and-drop, verify progress bar on large file, download a file
