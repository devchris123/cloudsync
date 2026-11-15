---
name: change
description: Implement CloudSync refactors, bug fixes, and features with evidence-based investigation, human design and review gates, focused verification, and branch/commit/PR completion.
---

# CloudSync change

Use one mode: **refactor**, **bugfix**, or **feature**. Select from the original task; clarify only when the difference affects scope. Do not apply this workflow to unrelated repositories. Read paths below relative to the repository root unless linked relative to this file.

## Coordinate and investigate

1. Preserve the original task verbatim, subsequent user clarifications, mode, acceptance criteria, and explicit exclusions. Establish affected client/server/storage/authentication boundaries.
2. Read applicable AGENTS.md files; inspect branch, HEAD, status, and diff. Identify user-owned changes, including untracked files; never stage or alter them incidentally. Confirm the default branch and create/use a task branch; never commit on main/default. Do not reset unrelated work.
3. Read [project evidence](references/project.md) and relevant source/tests/history. Revalidate claims against this checkout. Distinguish **requirement**, **plan**, **implementation**, **executed test**, **verified runtime observation**, **supplied investigation**, and explicitly labeled **inference**. A checked-in test is implementation evidence until run. Record command/result and limitations when asserting verification. Unsupported historical examples: **NONE FOUND**. Do not promote drafts or roadmap entries to shipped policy.
4. Understand the affected end-to-end flow and failure boundaries before proposing changes. Read the mode section below. Investigate useful facts before asking for decisions; do not ask all unresolved project questions on every task.

## Understand first by mode

- **Refactor:** write the behavior contract (API/signatures, serialized forms, CLI/output, errors, side effects, ordering and ownership where affected). Establish green characterization coverage before changing implementation; add missing characterization tests first. Issue #24 excluded signature/API/behavior changes and MockClient redesign, and kept tests co-located; 9e55753 implemented the split. For extraction, inventory satellites: tests, errors, DTOs, module/visibility ownership, and dead facades. Mark each **cheap-move-now** or **expensive-defer**, with a reason and explicit owner/follow-up. Do not silently turn a refactor into policy redesign.
- **Bugfix:** reproduce the actual invariant failure from evidence. Add a regression that fails before the fix and passes after; capture both outcomes. Prefer the failing storage/sync layer to incidental HTTP errors. Example: 5e7271a seeded valid existing blob bytes, supplied wrong chunks under the same expected hash, and asserted the original bytes survived reassembly. If faithful reproduction is unavailable, disclose that before promising a fix.
- **Feature:** establish the user driver, smallest valuable version, non-goals, acceptance criteria, and observable verification. Plans may deliberately narrow scope: the initial web UI plan chose read-only browsing; issue #29 chose logs before traces/metrics/alerts. Recheck present implementation rather than treating these old plans as permanent bans.

Pure logging, documentation, and configuration changes do not require new behavior tests; still run relevant validation. Use the lowest testing tier that proves the contract.

## GATE 1 — design, before coding

Pause and ask the human after investigation and before implementation. Present the contract, proposed minimal scope, evidence, verification plan, and only material forks with concrete alternatives and consequences. Even if no material fork exists, say so and ask approval of the proposed scope. Wait for an actual reply; elapsed time or tool auto-approval is not design approval.

Material forks include compatibility/migration/fresh state; deletion/conflict/retention policy; transaction ownership and filesystem coordination; tenant isolation/deduplication; quota/reservations/retained bytes; crate versus custom logic or browser tooling; and authentication trust boundaries. Use the scoped precedents in the references. Do not silently answer unresolved product policy.

## Implement minimally

After approval, make the smallest change satisfying the accepted contract. Follow observed Rust patterns and existing boundaries. Avoid unrelated cleanup, hypothetical requirements, and framework redesign. Keep invariants from the project reference explicit where affected. Do not fight format-on-write hooks: inspect semantic changes and flag cosmetic churn once (.vscode/settings.json enables format-on-save).

Use co-located unit tests, SyncApi mocks, tempdirs and temporary redb databases before broader integration tests when they prove the behavior. Use integration/runtime verification when required by the acceptance criteria. For Rust changes, run relevant tests and the checked-in CI expectations: `cargo fmt --check`, `cargo clippy -- --deny warnings`, `cargo test`; use `cargo build` where relevant. Explain failures/blockers without claiming success. For instructions/docs-only work, validate links, structure, and workflow semantics; application tests are normally unnecessary. Never claim the baseline investigation ran tests.

## Review, filter, and prioritize

Read [fixed review perspectives](references/review.md). Reviewers receive the **diff and original task plus human clarifications**, never the coder's narrative, proposed fix explanation, or self-review conclusions. Supply neutral repository locations, applicable instructions, and review rubric; let reviewers inspect source themselves.

When sub-agents are available **and applicable session instructions authorize them**, run reviewers as separate agents with fresh/minimal context, not a fork containing the coder narrative. Bounded concurrency is fine. Otherwise run the same fixed lenses sequentially and disclose the lack of independence. If a built-in correctness reviewer exists, delegate correctness to it and use custom reviewers for the remaining lenses; otherwise include correctness in the custom roster. Do not assume a named tool exists.

Require concrete trigger, consequence, evidence/location, severity, and proportionate remedy for every finding. No invented “also handle X” requirements. Filter for validity, reproduction, task relevance, duplication, and severity. Distinguish confirmed bugs, unverified concerns, and suggestions; retain only useful findings. Respect recorded scope boundaries while flagging omissions that demonstrably entrench debt.

## GATE 2 — triage, before review-driven changes

Pause with filtered findings, supporting evidence, and proposed **fix now / follow-up / reject** dispositions. Include the reason and priority. If none survive, report **NONE FOUND** and ask to proceed to finish. Wait for the human before applying any review-driven changes or filing follow-ups. Batch decisions to minimize prompts; do not waive either human gate because tool approvals are automatic.

Apply only agreed fixes, rerun affected checks, and review material new changes. Return to design gate for new material forks or scope changes; return to triage for new findings. Do not loop cosmetic suggestions indefinitely.

## Follow-ups and finish

Search existing issues in `devchris123/cloudsync` before filing agreed follow-ups. Reuse matching issues; otherwise file a concrete trigger/consequence, evidence, deferred remedy, acceptance criteria, and relationship to the current PR. Do not silently implement deferred work. Issue/PR writes and publishing require task authorization and current tool permissions; a skill alone does not grant it. If blocked, preserve the prepared content and report exactly what remains.

Confirm the task branch and stage only owned files. Follow CLAUDE.md's commit convention: capitalized imperative subject, about 50 characters, no period; brief body explaining why, motivation and trade-offs. Ask if motivation is unclear. Commit and create a PR when authorized; do not merge or deploy without authorization. Use `.github/pull_request_template.md`: Short description, Why, Approach, Alternatives considered; `closes: #<issue>` only for an issue actually resolved. No invented title prefix, labels, project assignment, or branch-protection guarantee. CI targets main and aggregates ci-pass; check actual results rather than assuming a markdown-only PR ran checks. Publishing release artifacts is separate from manual deployment.

Finish with changes, validation/results and limits, remaining risks/follow-ups, branch, commit, and PR link. Preserve original user work. Invocation example: [change prompt](prompts/change.md).
