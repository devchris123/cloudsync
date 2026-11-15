# CloudSync Codex guidance

For CloudSync refactors, bug fixes, and features, use the repository's
[change skill](.agents/skills/change/SKILL.md), invoked as `$change` plus the task.
It defines refactor / bugfix / feature modes, investigation, two human gates,
review, verification, and branch/commit/PR completion. Read-only questions do
not require the change workflow.

A reusable invocation prompt lives at
[change prompt](.agents/skills/change/prompts/change.md).
Keep application changes scoped to the user's task; skill maintenance alone
must not modify application code. Preserve unrelated working-tree files.
