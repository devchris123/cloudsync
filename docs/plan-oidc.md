# OIDC Authentication for CloudSync

## Context

CloudSync currently uses a single static token shared by all users. There is no user identity, no per-user isolation, and no way to restrict individual access. This plan introduces OIDC authentication via Keycloak, a user registry with auto-provisioning, and tenant-scoped file namespacing — transforming CloudSync from a single-user tool into a proper multi-user, multi-tenant system while keeping the static token as a migration fallback.

### Tenancy model

All data is scoped to a tenant from the start. `tenant_id` and `user_id` are always separate values — even when there's only one user per tenant, the tenant gets its own generated ID (nanoid). This means:
- Code never assumes `tenant_id == user_id`
- Adding a second user to an existing tenant is just an insert into the users table
- The user→tenant mapping lives in `UserRecord` and is the single source of truth

### Keycloak as the gatekeeper

Keycloak has self-registration **disabled by default**. Only users manually created by the admin (or approved via the "First Broker Login" flow for federated providers) can authenticate. This means Keycloak itself acts as the allowlist — if someone can get a JWT, they've been explicitly approved.

CloudSync auto-provisions a `UserRecord` (with a new tenant) on first login from a valid JWT. The `TABLE_USERS` table is not an allowlist — it's a **user→tenant mapping** and a **kill switch** (`is_active`) independent of Keycloak.

User control flow:
1. Admin creates user in Keycloak admin console (or approves federated login)
2. User authenticates → gets JWT with `sub` (opaque UUID, no PII)
3. CloudSync sees valid JWT, auto-creates `UserRecord { user_id: sub, tenant_id: <new nanoid>, is_active: true }`
4. To revoke: set `is_active = false` in CloudSync (immediate) or delete the Keycloak account (permanent)
5. To share a tenant: admin reassigns a user's `tenant_id` to an existing tenant via the admin API

### Minimal PII

CloudSync stores only:
- `user_id` — the OIDC `sub` claim (opaque UUID, not PII)
- `tenant_id` — generated nanoid
- `is_active` — boolean

No email, no display name, no profile data. If needed for UI display, read from JWT claims at request time. Keeps us out of GDPR/privacy concerns.

---

## Phase 1: `UserContext` + Tenant-Scoped DB Namespacing

**Goal:** Replace unit-struct `AuthGranted` with a typed `UserContext { user_id, tenant_id }`. Scope all DB operations to a tenant. Migrate existing data. Static token still works — maps to a configurable default user/tenant.

This is the riskiest structural change but requires no new dependencies and is fully testable with the existing static token.

### Changes

**`crates/cloudsync-common/src/lib.rs`** — Add `tenant_id: String` and `user_id: String` to `FileMeta` with `#[serde(default)]` for backward compat during migration.

**`crates/cloudsync-common/src/upload.rs`** — Add `tenant_id: String` and `user_id: String` to `Upload` and `InitUploadRequest` (also `#[serde(default)]`).

**`crates/cloudsync-server/src/app.rs`**:
- Replace `struct AuthGranted;` (line 231) with:
  ```rust
  #[derive(Clone)]
  pub struct UserContext {
      pub user_id: String,
      pub tenant_id: String,
  }
  ```
- Add `default_user_id: String` and `default_tenant_id: String` to `AppState`
- `bearer_auth_layer`: insert `UserContext { user_id: default_user_id, tenant_id: default_tenant_id }` instead of `AuthGranted`
- `cookie_auth_layer`: same
- `require_auth_layer`: check for `UserContext` instead of `AuthGranted`
- All handlers (`list_files`, `post_file`, `delete_file`, `get_file`, `create_upload`, `replace_chunk`, `get_upload`, `finalize_upload`): extract `UserContext` from request extensions, pass `tenant_id` to DB functions

**`crates/cloudsync-server/src/db.rs`** — Change DB key from `path` to `"{tenant_id}\0{path}"` (NUL separator):
- All functions (`list`, `get`, `put`, `delete`) accept `tenant_id: &str`
- `list` does prefix scan on `"{tenant_id}\0"` to return only that tenant's files
- Add migration in `open_db`: detect old-style keys (no NUL), rewrite to `"{default_tenant_id}\0{path}"`. Track schema version in a `TABLE_META` table.

**`crates/cloudsync-server/src/db_upload.rs`** — Store `tenant_id` and `user_id` in Upload value. Add ownership check on `get` (verify `tenant_id` matches).

**`crates/cloudsync-server/src/config.rs`** — Add `default_user_id: String` and `default_tenant_id: String` to `ServerConfig`.

**`crates/cloudsync-server/src/cli.rs`** — Add `--default-user-id` / `CLOUDSYNC_DEFAULT_USER_ID` (default: `"default"`) and `--default-tenant-id` / `CLOUDSYNC_DEFAULT_TENANT_ID` (default: `"default-tenant"`). Separate IDs from the start.

**`crates/cloudsync-server/src/ui.rs`** — `browse` handler passes `default_tenant_id` to `db::list` (upgraded to real identity in Phase 4).

### Content-addressable storage
Stays global/shared. Dedup across tenants is a feature. The metadata layer (DB keys) provides tenant isolation. Blobs are just hashes — they don't leak ownership.

### Testing
- Existing tests pass (static token maps to default user/tenant)
- New unit tests: composite-key CRUD, prefix-scan isolation between tenants, migration from old schema

---

## Phase 2: User Registry + Auto-Provisioning

**Goal:** `TABLE_USERS` stores the user→tenant mapping. Auto-provision on first OIDC login. Admin API for managing users and tenant assignments. Default user seeded on first boot.

Note: Keycloak controls who can authenticate (see "Keycloak as the gatekeeper" above). `TABLE_USERS` is not an allowlist — it's the registry that maps users to tenants and provides a kill switch.

### Changes

**`crates/cloudsync-server/src/db_users.rs`** (new) — `TABLE_USERS: TableDefinition<&str, &[u8]>`:
```rust
pub struct UserRecord {
    pub user_id: String,
    pub tenant_id: String,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}
```
CRUD: `list_users`, `get_user`, `create_user`, `deactivate_user`.

**`crates/cloudsync-server/src/db.rs`** — Open `TABLE_USERS` in `open_db`. Seed default user with a separate `default_tenant_id` if table is empty.

**`crates/cloudsync-server/src/app.rs`**:
- Add `user_registry_layer` middleware: after auth layers, before `require_auth_layer`. Looks up `UserContext.user_id` in `TABLE_USERS`:
  - **Found + active**: populate `UserContext.tenant_id` from the `UserRecord` (authoritative source)
  - **Found + inactive**: return 403 (kill switch)
  - **Not found**: auto-provision — create `UserRecord` with new `tenant_id` (nanoid), populate `UserContext`
- Add admin API routes (gated by admin user_id check). These manage **tenant assignments**, not user identity (that's Keycloak's job):
  - `GET /api/v1/admin/users` — list all users and their tenant assignments
  - `PUT /api/v1/admin/users/{user_id}` — update user (reassign tenant, toggle active)
  - `GET /api/v1/admin/tenants/{tenant_id}/users` — list users in a tenant

**`crates/cloudsync-server/src/cli.rs`** — Add `--admin-user-id` / `CLOUDSYNC_ADMIN_USER_ID` (defaults to `default_user_id`).

### Testing
- Unit tests for `db_users.rs`
- Integration test: deactivate a user, verify 403

---

## Phase 3: JWT Validation + OIDC Discovery (Server)

**Goal:** Server validates JWT access tokens from Keycloak. Static token fallback remains. Server-only — client continues with static tokens.

### Changes

**`crates/cloudsync-server/src/oidc.rs`** (new):
- `OidcValidator` struct: cached JWKS keys in `RwLock<HashMap<String, DecodingKey>>`, reqwest client
- `discover(issuer_url)`: fetch `.well-known/openid-configuration`, extract `jwks_uri`, fetch JWKS
- `validate_token(token) -> Result<Claims>`: decode JWT, validate `exp`/`iss`/`aud`, extract `sub`/`email`
- JWKS refresh on `kid` miss (single retry)

**`crates/cloudsync-server/src/app.rs`**:
- Add `oidc: Option<Arc<OidcValidator>>` to `AppState`
- `bearer_auth_layer`: try static token first (fast path), then JWT validation. On valid JWT, insert `UserContext { user_id: claims.sub, tenant_id: "" }` (placeholder — `user_registry_layer` from Phase 2 resolves the real `tenant_id` from the `UserRecord`)

**`crates/cloudsync-server/src/config.rs`** — Add `oidc_issuer_url: Option<String>`, `oidc_audience: Option<String>`.

**`crates/cloudsync-server/src/cli.rs`** — Add `--oidc-issuer-url` / `CLOUDSYNC_OIDC_ISSUER_URL`, `--oidc-audience` / `CLOUDSYNC_OIDC_AUDIENCE` (optional).

**`crates/cloudsync-server/src/main.rs`** — If OIDC env vars set, call `OidcValidator::discover()` at startup.

### New dependencies (server)
- `jsonwebtoken = "9"` — JWT decoding/validation
- `reqwest` (add to server runtime deps) — OIDC discovery, JWKS fetch

### Testing
- Unit test with locally-generated RS256 keypair + hand-crafted JWT
- Integration test with mock JWKS endpoint
- Verify static token still works alongside OIDC

---

## Phase 4: Web UI OIDC (Authorization Code + PKCE)

**Goal:** Replace token login form with "Sign in with Keycloak" using Authorization Code + PKCE flow. Session cookies now carry user identity.

### Changes

**`crates/cloudsync-server/src/ui.rs`**:
- Login page: if OIDC configured, show "Sign in" button → redirect to Keycloak authorize endpoint with `state` + PKCE `code_verifier` (stored in encrypted cookie or server-side map)
- New `GET /auth/callback` handler: exchange code for tokens at Keycloak token endpoint, validate ID token, extract user identity
- Session cookie format changes from `{hmac_sig}` to `{user_id}:{hmac(user_id, secret)}` — `cookie_auth_layer` extracts `user_id`, `user_registry_layer` resolves `tenant_id`
- Fallback: if OIDC not configured, keep existing token form

**`crates/cloudsync-server/src/config.rs`** — Add `oidc_client_id_web: Option<String>`, `oidc_client_secret_web: Option<String>`.

**`crates/cloudsync-server/templates/login.html`** — Conditional rendering: OIDC button vs token form.

### Keycloak web client config
- Client type: OpenID Connect, confidential
- Valid redirect URI: `https://cloudsync.example.com/auth/callback`
- Standard flow: enabled

### Testing
- Unit test cookie format parsing/verification
- Manual test with running Keycloak

---

## Phase 5: Client OIDC (Device Authorization Grant)

**Goal:** CLI authenticates via OIDC Device Authorization Grant. Tokens stored and refreshed automatically.

### Changes

**`crates/cloudsync-client/src/auth.rs`** (new):
- `DeviceAuthFlow`: POST to device authorization endpoint, get `device_code` + `user_code` + `verification_uri`. Poll token endpoint. Open browser with `open` crate.
- `TokenStore`: persist `access_token`, `refresh_token`, `expires_at` in `.cloudsync/tokens.json` (file permissions `0600`). Transparent refresh on `get_bearer_token()`.
- `AuthProvider` enum: `StaticToken(String) | Oidc(TokenStore)` — unified `get_bearer_token()` method.

**`crates/cloudsync-client/src/config.rs`** — `token` becomes `Option<String>`. Add `oidc_issuer_url: Option<String>`, `oidc_client_id: Option<String>`.

**`crates/cloudsync-client/src/cli.rs`**:
- `Init`: `--token` becomes optional. Add `--oidc-issuer-url`, `--oidc-client-id`.
- Add `Login` subcommand: triggers device auth flow interactively.

**`crates/cloudsync-client/src/client.rs`** — `SyncClient` takes `AuthProvider` instead of raw token. Each request calls `auth_provider.get_bearer_token()`.

**SyncApi trait: unchanged.** Auth is internal to `SyncClient`.

### Keycloak CLI client config
- Client type: OpenID Connect, public (no secret)
- Device Authorization Grant: enabled
- Client ID: `cloudsync-cli`

### New dependencies (client)
- `open = "5"` — open verification URL in browser

### Testing
- Mock device auth + token endpoints
- Test refresh flow, expired refresh → re-login prompt

---

## Phase 6: Keycloak Deployment

**Goal:** Add Keycloak to Docker Compose stack. Provide reproducible setup.

### Changes

**`docker-compose.yml`** — Add `keycloak` service (quay.io/keycloak/keycloak:26.2). Add OIDC env vars to `cloudsync` service.

**`caddy/Caddyfile`** — Add `auth.cloudsync.example.com` vhost → `keycloak:8080`.

**`keycloak/realm-export.json`** (new) — Pre-configured realm with 3 clients (`cloudsync-server`, `cloudsync-web`, `cloudsync-cli`). Import on startup for zero-config.

**`docs/keycloak-setup.md`** (new) — Step-by-step guide: realm creation, client config, user provisioning, token lifetime tuning (access: 5min, refresh: 30 days).

**`docs/server-setup.md`** — Update with Keycloak deployment steps.

---

## Phase 7 (future): Remove Static Token Fallback

Not in initial scope. Once all users migrated: remove `token` from config, make OIDC required. Breaking change — separate release.

---

## Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| Tenant-scoped from the start | Avoids painful migration when multi-tenancy (orgs, shared workspaces) is needed later. `tenant_id` is always a separate generated ID, never equal to `user_id`. |
| Composite DB key `"{tenant_id}\0{path}"` | redb table names are compile-time constants, so separate tables per tenant is not possible. NUL separator is unambiguous and sortable for prefix scans. |
| Content-addressable storage stays shared | Dedup across tenants saves space. Metadata layer provides isolation. Hash does not leak ownership. |
| `sub` claim as `user_id` | Stable, unique OIDC identifier. Email can change. |
| `tenant_id` resolved from `UserRecord` | Auth layer provides a preliminary `tenant_id`, but the user registry layer overrides it from the DB. This keeps tenant assignment authoritative and centralized. |
| Static token checked before JWT | Fast path for existing setups. Avoids crypto validation on every request when not using OIDC. |
| `jsonwebtoken` over `openidconnect` crate | Much lighter. We only need discovery + JWKS + JWT validation. |
| Device Authorization Grant for CLI | Correct OAuth2 flow for CLI tools — no browser redirect, no local HTTP server. |
| Keycloak as gatekeeper, not CloudSync | Keycloak controls who can authenticate (self-registration off, admin creates accounts). CloudSync auto-provisions on first valid JWT. `TABLE_USERS` is a mapping + kill switch, not an allowlist. |
| Minimal PII | Only store `user_id` (opaque UUID `sub`), `tenant_id`, and `is_active`. No email or profile data persisted. Read from JWT claims at request time if needed for display. |

## Verification

After each phase, verify with:
```bash
cargo build                        # Compiles
cargo test                         # All tests pass
cargo clippy -- --deny warnings    # No warnings
```

End-to-end after Phase 6:
1. `docker compose up` — Keycloak + CloudSync start
2. Import realm, create test user in Keycloak
3. CLI: `cloudsync init --server-url ... --oidc-issuer-url ... --oidc-client-id cloudsync-cli`
4. CLI: `cloudsync login` — device flow, approve in browser
5. CLI: `cloudsync push` / `cloudsync pull` — files scoped to tenant
6. Web UI: "Sign in with Keycloak" → browse tenant's files
7. Admin: add second user (own tenant), verify namespace isolation
