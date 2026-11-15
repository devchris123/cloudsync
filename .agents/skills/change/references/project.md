# Project evidence and boundaries

## Provenance

Revalidation baseline: main, b0a1962be720e614b3b7edf82ac788c38f58faa1, 2026-11-15. Skill construction changed no application code and ran no application tests. Recheck the future checkout before relying on this snapshot.

Directly inspected: README, manifests, CLAUDE.md, toolchain, CI, PR template, editor settings, sync core/push/pull, scanner/client DB, common FileMeta, storage, GC, migrations, OIDC/service/app source excerpts; commit 9e55753 and 5e7271a descriptions; live GitHub issue bodies #24, #48, #59. This establishes implementation, checked-in tests, historical rationale, and issue requirements—not runtime verification. Other issue/PR/commit examples below originate in the user's supplied read-only investigation; inspect the originals before making them task requirements. Source paths refer to the repository root. Use `git show <commit>` and `gh issue view <n>` / `gh pr view <n>` when available. Tool failures are not evidence of absence.

At baseline untracked `docs/issues/`, `docs/todo`, and `transaction-design-follow-up.md` were user-owned drafts, not authoritative shipped policy. Preserve them and recheck status. CLAUDE.md's global CAS layout and old toolchain/current-state passages are stale; retain its relevant conventions without copying stale architecture.

## Architecture and flows — implementation evidence

CloudSync provides manual CLI push/pull/status and a server-rendered browser UI, not a background daemon (README). Common owns serialized metadata/upload types and BLAKE3; client owns Clap CLI, scanner/ignores, HTTP, sync, redb state and OIDC login/refresh; server owns Axum API, tenant-scoped service/repositories, filesystem blobs, auth/UI/GC.

Tokio, Axum, reqwest/rustls, redb, Serde/JSON, BLAKE3, tracing, Askama, rust-embed and vendored browser JS are present. Deployment uses Caddy TLS termination and Keycloak OIDC (manifests, docs/server-setup.md, docs/plan-web-ui.md). Dependency presence alone does not prove selection rationale.

- **Push:** scanner respects ignores; compare local hashes against SyncRecord; small direct uploads or 4 MiB chunks; persist upload IDs, query received chunks, skip completed chunks, restart expired sessions; finalize and save returned version. Missing tracked local files trigger server deletes. Sources: client `scanner.rs`, `db.rs`, `sync/{core,push}.rs`; historical #18/cfd01b8.
- **Publication:** authenticate UserContext, enforce tenant/quota/reservations, write content-addressed bytes, update metadata/accounting through redb transactions. Filesystem operations and DB commits are distinct failure boundaries (`server/src/service.rs`).
- **Pull:** compare listed server versions/local hashes; stream/resume into `.part`, verify BLAKE3, rename, update state. Conflicts preserve local bytes and save server bytes beside them with a timestamped conflict name (`sync/pull.rs`, 6efb0e0, #19/4eb6354). Do not infer a complete remote-deletion/local-edit contract from this.
- **Storage:** `server/src/storage.rs` uses `{data_dir}/{BLAKE3(namespace)}/{hash[0..2]}/{hash}`. Direct write uses `std::fs::write`. Chunk assembly uses a temporary file, size/hash checks, then persist; existing immutable content must survive reassembly.
- **GC:** `service_gc.rs` persists deletion intent, protects live references/active uploads, unlinks, commits accounting exactly once; completion logs follow commits. Tests cover unlink-before-accounting, preserved intent and partial-failure retries. Current cleanup has no recovery retention window (#59 describes delivered behavior). Tombstones differ from retained bytes/history. Retention, replacement history/orphans and upload expiry remain separately scoped (#53–#59).

## Human decisions — scoped precedents

| Material fork | Grounding and limit |
| --- | --- |
| Existing DB/index/client compatibility versus fresh state | c77a33a and docs/plan-oidc.md use Serde defaults, tenant-key migration and schema versioning. Current server migrations live in `server/src/migrations/`. #59 excludes index backfilling under a fresh-database decision; it does not authorize dropping compatibility elsewhere. |
| Conflict/deletion/retention policy | 6efb0e0 preserves both conflicting contents; inspect pull plus #54–#57. #54's intended retention and #59's delivered immediate reclamation differ. Ask for the task's policy. |
| Transaction ownership/FS coordination | #48 rejects a larger store abstraction as too complex, explains read/write redb type limits and the limited enum trade-off; #49 selected approach, `server/src/db/tx.rs`. Lock duration must preserve quota/GC guarantees. |
| Isolation versus deduplication | #39/#44 deliberately sacrifice cross-tenant deduplication for privacy, isolation and simpler accounting. |
| Quota/reservations/retained bytes | #47 atomic usage/reservations/metadata; #46 deferred duplicate-upload reservation improvements; #59 retained-content scope. |
| Dependency versus custom logic | #9 reqwest/rustls avoids OpenSSL cross-compilation; web UI plan selects Askama/pure Cargo/single binary over React/Vite for initial read-only scope. These are proportionality examples, not bans. |
| Auth trust boundary | 18b0e4b separates private signing secret from distributed bearer token; 00e8a22 separates public issuer/internal discovery. Inspect fallback removal and cookie invalidation consequences. |

Open questions remain task-specific: compatibility including mixed clients/downgrades; tombstone versus local modifications; retention versus immediate reclamation; intended minimum Rust; additional branch/PR conventions; scope boundaries especially #43, #46, #55–#59. Do not require answers to irrelevant questions.

## Integrity and compatibility checklist — apply affected invariants

- Never append new reassembly bytes to an existing immutable blob (a171dd1 doubling incident; 5e7271a sharp regression extraction). Validate content before publication and `.part` before rename. Do not describe all writes as atomic or universally crash-durable.
- Preserve conflict bytes, tenant-scoped metadata keys (`server/src/db/db_file.rs`), and namespace-hashed storage. Keep required quota/reservations/metadata changes atomic and deletion intent recoverable; account once.
- Keep signing secrets server-private. #38's credential reuse and 18b0e4b correction are a credential/abstraction shortcut with follow-up pain, not a schema-migration incident. Enforce auth checks and trust configuration; OIDC hardening plan requires algorithm whitelist plus issuer/audience/time validation. CLAUDE.md treats discovery configuration as integrity-critical and forbids disabling TLS verification. Separate plan requirements from inspected implementation.
- FileMeta is shared wire/server redb metadata; client redb JSON encodes SyncRecord (`common/src/lib.rs`, client `db.rs`, `sync/core.rs`). Changes can affect both storage and protocol. OIDC plan calls for `#[serde(default)]`, `tenant_id\0path` keys and schema-version migration. Client config retains optional legacy static token; OIDC plan assigns removal to a separate breaking release (#38, client `config.rs`).
- BLAKE3 is hashing, not encryption. E2E encryption, Argon2, XChaCha20-Poly1305/AES-256-GCM in docs/roadmap.md are proposals, not implemented file encryption guarantees.

## Testing and delivery evidence

Existing tests use co-located `#[cfg(test)]`, Tokio, tempdirs, real temporary redb DBs, and SyncApi/MockClient. Storage tests avoid HTTP/auth scaffolding. Client integration tests launch loopback Axum and check push/pull bytes/conflicts; server integration tests cover chunks/Range; cookie_signing_test.rs and tenant_limit_test.rs cover their boundaries. GC tests simulate interrupted deletion/accounting. 9d01613 replaces process-global test environment mutation with a private override to avoid concurrent interference.

CI runs rustfmt, `cargo clippy -- --deny warnings`, `cargo test`, release client builds for Linux/macOS x86_64/aarch64, with ci-pass aggregation. Docs and markdown paths skip it. `rust-toolchain.toml` pins **1.99.0**; README/CLAUDE still say **1.91.1**. The pin is configuration, not independent proof of MSRV support. `.github/pull_request_template.md` supplies finish sections. Release publishes artifacts; deploy workflow is manual.

## Unsupported conventions — NONE FOUND

Explicit PR criticism for excessive size/review difficulty; explicit prior-PR criticism for solving the wrong problem; Rust unsafe examples in searched crate sources; visibility policy beyond observed modules; blanket production unwrap/expect prohibition (both exist); unique end-to-end sync correlation ID; outage specifically motivating exponential backoff; established general automatic transfer retry/backoff; dependency audit/maintenance policy; universal fsync/crash durability; golden-file/fixture-directory convention; comprehensive old/new coexistence matrix, protocol negotiation or downgrade guarantee; independent MSRV matrix; mandatory PR title prefix/labels/project assignment; verified branch-protection settings. `/api/v1` proves none of those compatibility guarantees.
