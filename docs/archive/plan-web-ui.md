# Phase 3: Web UI — Read-Only File Browser

## Context

CloudSync is CLI-only today. Phase 3 adds a Web UI. We start with a **read-only file browser** — login, browse directories, view file metadata, download files. Upload and delete come later as incremental additions once the foundation is solid.

## Approach: Askama + Vanilla CSS (instead of React + Vite)

The roadmap proposed React + TypeScript + Vite embedded via `rust-embed`. After analysis, **Askama server-rendered templates** are a better fit for the read-only scope:

| Criteria | React + Vite | Askama + CSS |
|----------|-------------|--------------|
| Build dependencies | Adds Node.js to CI + Docker | Zero change — pure `cargo build` |
| Tooling overhead | ~200MB node_modules, package.json, vite.config, tsconfig | None |
| JavaScript needed | Yes (SPA) | None for read-only scope |
| Single-binary deploy | Yes (via rust-embed) | Yes (via rust-embed) |
| Proportionality | ~500 lines UI code for file browser | ~150 lines Rust + 3 HTML templates |
| Future upgrades | Already set up for richer UI | Add vanilla JS incrementally for upload/delete |

React makes sense if the UI grows into a full dashboard. For a file browser with download links, server-rendered HTML is the right tool.

---

## Implementation Steps

### Step 1: Dependencies and module structure

Add to `crates/cloudsync-server/Cargo.toml`:
- `askama = "0.13"` — compile-time HTML templates
- `askama_axum = "0.5"` — Axum responder integration
- `rust-embed = "8"` — embed static assets (CSS) into binary

Create files:
```
crates/cloudsync-server/
  templates/
    base.html           — layout shell (head, nav, CSS link)
    login.html          — token input form
    browser.html        — file table + breadcrumbs
  static/
    style.css           — minimal CSS
  src/
    ui.rs               — UI route handlers + RustEmbed struct
```

Wire `mod ui;` in `src/lib.rs`.

### Step 2: Prefix filtering on file listing

Modify `db::list()` in `src/db.rs` to accept an optional prefix parameter. Derive a directory listing from the flat file paths:
- Immediate subdirectories at the current prefix level
- Files at the current prefix level (not nested deeper)

Example: with files `a.txt`, `photos/1.jpg`, `photos/vacation/2.jpg` and prefix `photos/`:
- Directories: `["vacation/"]`
- Files: `[FileMeta for "photos/1.jpg"]`

Expose on the API too: `GET /api/v1/files?prefix=photos/` — benefits future API clients.

**Files:** `crates/cloudsync-server/src/db.rs`, `crates/cloudsync-server/src/app.rs`

### Step 3: Cookie-based auth for browser sessions

Extend `auth_layer` in `app.rs` to accept either:
- `Authorization: Bearer <token>` header (existing — CLI and API clients)
- A signed `HttpOnly; SameSite=Strict` auth cookie (new — browser sessions)

Add handlers:
- `POST /login` — validate token, set cookie, redirect to `/browse`
- `POST /logout` — clear cookie, redirect to `/login`

The cookie contains an HMAC signature derived from the token, enabling stateless verification without a session store.

**Files:** `crates/cloudsync-server/src/app.rs`, `crates/cloudsync-server/src/ui.rs`

### Step 4: Templates and UI routes

**Templates (Askama):**
- `base.html` — HTML5 shell, embedded CSS link, nav bar with "CloudSync" title
- `login.html` — extends base, single text field for bearer token, POST form
- `browser.html` — extends base, breadcrumb nav (prefix split on `/`), table with columns: name, size, modified date. Directories link deeper (`/browse?prefix=dir/`). Files have download links.

**Routes:**
- `GET /` — redirect to `/browse` (if auth cookie present) or `/login`
- `GET /login` — render login page
- `GET /browse?prefix=` — render file browser (requires auth cookie)
- `GET /static/{*path}` — serve embedded CSS
- `POST /login` — validate + set cookie
- `POST /logout` — clear cookie

**Files:** `crates/cloudsync-server/src/ui.rs`, `templates/`, `static/`

### Step 5: File downloads from browser

Each file row in the browser template links to `/api/v1/files/{path}`. Since `auth_layer` now accepts cookies (Step 3), the browser sends the auth cookie automatically and the file streams down — no JavaScript needed.

**No new code** — this works from the template links + auth middleware change.

---

## Files to Modify

| File | Change |
|------|--------|
| `crates/cloudsync-server/Cargo.toml` | Add askama, askama_axum, rust-embed |
| `crates/cloudsync-server/src/lib.rs` | Add `mod ui;` |
| `crates/cloudsync-server/src/app.rs` | Add UI routes, extend auth_layer for cookies |
| `crates/cloudsync-server/src/db.rs` | Add optional prefix filtering to `list()` |
| `crates/cloudsync-server/src/ui.rs` | **New** — UI handlers, RustEmbed struct, template structs |
| `crates/cloudsync-server/templates/*.html` | **New** — 3 Askama templates |
| `crates/cloudsync-server/static/style.css` | **New** — minimal CSS |

## Unchanged

- **Dockerfile** — no new build stages needed
- **CI** — no new steps
- **cloudsync-common** — shared types unchanged
- **cloudsync-client** — CLI unaffected
- **Existing API** — fully backward compatible

---

## Verification

1. `cargo build` — compiles with templates + CSS embedded
2. `cargo test` — existing tests still pass
3. `cargo clippy -- --deny warnings` — no new warnings
4. Start server: `cargo run -- --token test123`
5. Open `http://localhost:3050/` — redirects to `/login`
6. Enter token — redirects to `/browse`
7. File browser shows files with name, size, date
8. Click a directory — breadcrumb updates, shows nested contents
9. Click a file — browser downloads it
10. CLI `push`/`pull` still works (API unchanged)

---

## Future Increments (not in scope)

- **Upload**: drag-and-drop + file picker, chunked upload with progress bar (vanilla JS)
- **Delete**: per-file button with confirmation dialog (vanilla JS)
- **Dashboard**: file count, storage usage, server uptime
