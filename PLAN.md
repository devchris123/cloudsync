# CloudSync — Personal Cloud File Storage with Sync

## Context

Build a personal cloud file storage system from scratch in Rust. A self-hosted alternative to Dropbox/Nextcloud that's lightweight, with a CLI-driven manual sync workflow. Both a practical tool and a Rust learning project.

**Decisions:** Self-hosted server, manual CLI sync (push/pull/status), minimal-but-solid first version.

## Architecture

**Cargo workspace** with 3 crates:

```
cloudsync/
  Cargo.toml              # workspace root
  crates/
    cloudsync-common/     # shared types, protocol, hashing
    cloudsync-server/     # axum HTTP server + redb + file storage
    cloudsync-client/     # CLI tool (clap) + sync engine + local state
```

### Core Data Model (`cloudsync-common`) — shared types only
- `FileMeta` — path, size, content_hash (blake3), version counter, is_deleted, timestamps
- `FileAction` — enum: Created, Modified, Deleted, Unchanged
- Protocol types for API request/response serialization
- `hash_file()` / `hash_bytes()` utilities using blake3

### Server (`cloudsync-server`)
- **axum** REST API with token auth middleware (`Authorization: Bearer <token>`)
- **Endpoints:** `GET /api/files` (list), `GET /api/files/:path` (download), `POST /api/files` (upload multipart), `DELETE /api/files` (mark deleted), `GET /api/health`
- **Content-addressable file storage** on disk (`files/{hash[0..2]}/{hash}`) — prevents path traversal, deduplicates
- **redb** metadata DB tracking path → hash/version/timestamps (serde-serialized structs, no schema migrations)
- Server version counter per file (monotonic, avoids clock-skew issues)

### Client (`cloudsync-client`)
- `SyncRecord` — client-only type: what the client last knew about a file (path, local_hash, server_version)
- `Conflict` — client-only type: both sides changed since last sync
- **CLI commands:** `init`, `push`, `pull`, `status`, `config set/show`
- `.cloudsync/` directory in sync root (like `.git/`) with `config.toml` + `sync.redb`
- `find_sync_root()` walks up directories to find `.cloudsync/`
- **Scanner:** recursive dir walk, computes blake3 hashes, diffs against last-known state
- **Push:** scan local → diff → upload new/modified → delete remote for locally-deleted
- **Pull:** fetch remote list → diff → download new/modified → detect conflicts
- **Conflicts:** keep both versions (local stays, remote saved as `file.conflict.<timestamp>.ext`)
- **HTTP client** wrapper using reqwest

### Key Dependencies
axum, tokio, clap, serde/serde_json, reqwest, redb, blake3, uuid, chrono, tracing

## Implementation Phases

### Phase 1: Project Scaffold
- Create workspace, all 3 crates, wire up dependencies
- Implement `cloudsync-common` fully (models, protocol, hash)
- **Verify:** `cargo build` + `cargo test`

### Phase 2: Server — Storage & Database
- `ServerDb` (redb): open, list/get/upsert/mark_deleted with serde-serialized FileMeta values
- `FileStorage`: content-addressable store/read/exists
- Unit tests with tempdir
- **Verify:** `cargo test -p cloudsync-server`

### Phase 3: Server — HTTP API
- Auth middleware, error types
- Route handlers: health, list, upload, download, delete
- `main.rs`: axum router, CLI args (--port, --token, --storage-dir)
- **Verify:** server starts, test with curl

### Phase 4: Client — CLI Scaffold & Config
- `ClientConfig` load/save, `find_sync_root()`
- `LocalDb` (redb sync state)
- Clap CLI with all subcommands
- `init` and `config` commands working
- **Verify:** `cloudsync init` creates `.cloudsync/`

### Phase 5: Client — Push
- `scanner.rs`: scan_local_files, diff_local
- `client.rs`: CloudClient HTTP wrapper
- `sync.rs::push`: full push algorithm
- **Verify:** end-to-end push to running server

### Phase 6: Client — Pull & Conflicts
- `conflict.rs`: conflict filename generation, save conflict file
- `sync.rs::pull`: full pull algorithm with conflict detection
- **Verify:** pull new files, pull updates, conflict scenario

### Phase 7: Client — Status
- `sync.rs::status`: compare local/sync/remote state
- Formatted CLI output showing local changes, remote changes, conflicts
- **Verify:** status output matches actual state

### Phase 8: Polish
- tracing logging throughout, --verbose flag
- Error message improvements
- Unit tests for scanner, sync (mock HTTP client via trait)
- Integration test: start server + run client against it

## Conflict Example

Suppose `notes.txt` was pushed to the server (version 1). Both sides are in sync.

1. You edit `notes.txt` locally (hash changes)
2. From another machine, a new `notes.txt` is uploaded to the server (version bumps to 2)
3. You run `pull`

Both sides changed since last sync — conflict detected.

**Resolution:** local file stays, server version saved alongside:
```
my-folder/
  notes.txt                              ← your local version (unchanged)
  notes.conflict.20260328T143000.txt     ← server's version
```
You manually decide what to keep.

**No conflict cases:**
- Only local changed → `push` overwrites server (local wins)
- Only server changed → `pull` overwrites local (safe, you didn't edit)
- Neither changed → nothing happens

**Note:** Conflicts are only detected on `pull`, not `push`. Push always overwrites the server with your local version.

## Verification
1. `cargo build` — workspace compiles
2. `cargo test` — all unit tests pass
3. Manual end-to-end: start server, init client dir, create files, push, modify on server via curl, pull, verify conflicts
4. `cargo clippy` — no warnings
