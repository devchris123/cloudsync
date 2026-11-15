# Fixed review perspectives

Run every lens; mark unaffected dimensions with a brief reason. Project paths are relative to repository root. Historical evidence not directly revalidated during construction is labeled in project.md; recheck relevant originals before relying on it.

## Reviewer packet and output

Provide original task and human clarifications, diff (including new files), relevant base/HEAD and repository access, applicable instructions, this rubric and relevant project evidence. Do not provide coder narrative, implementation rationale, prior findings or intended verdict. With agents, use fresh context and read-only review; never let reviewers edit or publish. Agent availability does not override session delegation restrictions. A built-in correctness reviewer receives the same neutral packet.

For each finding supply **trigger → consequence → evidence/location → proportionate remedy**, severity/priority, and reproduction status. Investigate actual callers/contracts before asserting breakage. Separate verified failures from plausible concerns. Report **NONE FOUND** where unsupported. Minimality applies to every lens: no hypothetical “also handle X” requirement without a concrete trigger. Recorded follow-ups matter; do not turn deferred policy into mandatory scope. Filter duplication across lenses before triage.

## 1. Focused / minimal

Check accepted scope, small self-contained changes, separation of unrelated concerns (CLAUDE.md). #24 excluded API/signature changes and MockClient redesign. #47 deferred reservation/API redesign to #46/#48. #59 requires contention measurements before batching/cursors/indexing. Flag omissions that demonstrably entrench debt, respecting follow-ups. Explicit too-large/hard-to-review PR example: **NONE FOUND**. #32 justified combining two fixes because either alone left the deployment path broken.

For extraction, inventory tests, errors, DTOs, module/visibility ownership, dead facades; classify cheap-move-now versus expensive-defer with ownership. Ground in #24/9e55753 and storage extraction 5e7271a. A thin facade needs a demonstrated ownership or compatibility reason.

## 2. Design forks

Check whether material alternatives reached the human before coding: compatibility/fresh-state, conflict/deletion/retention, transaction/FS coordination, isolation/deduplication, quota, dependency/browser tooling, auth trust. Use scoped precedents in project.md. Do not treat c77a33a migration or #59 fresh-index decision as universal. A hidden product-policy change inside a refactor is a finding. Do not demand decisions unrelated to the task.

## 3. Correctness / right layer

Trace the actual trigger and contract through the affected flow. a171dd1 identifies append-on-finalize to an existing CAS blob as the doubling cause; 5e7271a moves reassembly to cheaply test the invariant. A downstream HTTP failure alone is weak regression evidence. #32 identifies scp nesting and changed bind-mounted compose configuration not reloading; **inference:** application fixes would not address those deployment causes. #60/b0a1962 records manually discovered uninitialized tenant-table and seconds-as-milliseconds startup/GC defects. These historical reports do not prove this checkout was runtime-tested. Explicit previous-PR criticism for solving the wrong problem: **NONE FOUND**.

## 4. Style / maintainability / testability

Follow observed Rust: anyhow::Result, `?`, contextual errors, explicit boundary error enums/manual conversions. SyncApi and MockClient enable sync unit testing. Tests commonly unwrap; production unwrap/expect also exist, including path conversions—no established blanket prohibition. 9d01613 uses private test overrides instead of global environment mutation. GC spawn_blocking does not remove redb writer contention (#59). Unsafe examples / formal visibility policy: **NONE FOUND**. Avoid imposing a new framework as style cleanup.

## 5. Observability

Check whether changed behavior/failure can be distinguished with proportionate existing logs/progress. 91d09ce adds labeled production logs; app/service/GC use tenant, operation, blob hash, upload ID, committed outcomes and elapsed milliseconds; HTTP uses TraceLayer. GC success/reclamation outcomes follow accounting commits. CLI progress and per-file errors can coexist with continued processing. #29 proposes running-build checks in Grafana/Loki and deliberately defers traces/metrics/alerts. Unique sync-session correlation convention: **NONE FOUND**. Do not mandate new telemetry infrastructure.

## 6. Resiliency

Check affected interruption/retry paths and partial outcomes: persisted upload IDs/chunk queries/expired-session restart (#18/cfd01b8), Range resumes and verified/discarded partials (#19/4eb6354), deletion-intent recovery/retry tests in service_gc.rs, OIDC discovery/JWKS cache with one refresh on unknown kid. Existing general automatic transfer retry/backoff or outage motivating exponential backoff: **NONE FOUND**. #59 says 409 pending-deletion conflicts require a later retry; any new automation needs an agreed bounded policy. Do not invent infinite retry requirements.

## 7. Dependency versus DIY

Assess proportionality at the actual boundary. #9 chose reqwest/rustls for cross-compilation; UI plan chose Askama over React/Vite for pure Cargo/single binary; #48 chose a limited transaction enum over a larger store abstraction. jsonwebtoken/oauth2/hmac/sha2 presence does not establish author rationale. Maintenance/audit policy: **NONE FOUND**. No absolute ban on crates or JS. Bring a material new dependency/runtime/build-tool trade-off to the design gate.

## 8. Data integrity / safety

Require explicit affected-invariant analysis: immutable existing blob preservation; content verification before publish/rename; conflict preservation; tenant namespace/key isolation; atomic required quota/reservation/metadata changes; persisted recoverable deletion intent and exactly-once accounting; tombstones distinct from retention/history; private signing secrets and preserved authentication/trust checks. See project.md for sources and limits. BLAKE3 is not encryption, roadmap crypto is not shipped protection, direct storage write is not universally atomic, universal fsync guarantees are **NONE FOUND**.

## 9. Format / compatibility safety

Trace shared FileMeta wire/server DB and JSON SyncRecord client DB changes. Inspect defaults, composite tenant keys, migration versions and real affected existing data. c77a33a/OIDC plan establish one migration precedent; #59 establishes a narrower fresh-index exception. Legacy static-token config remains; OIDC plan assigns removal to a separate breaking release. Mixed-client matrix, negotiation and downgrade guarantees: **NONE FOUND**; `/api/v1` alone provides none. Ask for material compatibility policy rather than manufacturing it.
