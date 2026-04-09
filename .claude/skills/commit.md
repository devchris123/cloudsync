---
description: Create a git commit with best practices
user-invocable: true
---

# Commit Skill

Create a well-crafted git commit. Follow these steps exactly:

## Step 1: Understand the current state

Run these commands in parallel:

1. `git status` — see all changed/untracked files
2. `git diff` and `git diff --staged` — see actual changes (staged and unstaged)
3. `git log --oneline -10` — see recent commit style

## Step 2: Evaluate commit scope

- If the changes span multiple unrelated concerns, ask the user whether to split into multiple commits. Each commit should be **small, self-contained, and leave the code in a working state**.
- Do NOT commit files that likely contain secrets (.env, credentials, tokens).

## Step 3: Stage files

- Stage only the files relevant to this commit by name. **Never use `git add -A` or `git add .`**.

## Step 4: Write the commit message

**Subject line:**
- Concise one-liner, max ~50 characters
- Imperative mood ("Add feature", not "Added feature" or "Adds feature")
- No trailing period
- Capitalize the first word

**Body (separated by blank line):**
- Explain the **rationale** — WHY this change was made
- Do NOT repeat what was changed; the diff already shows that
- Focus on motivation, context, trade-offs, or consequences that aren't obvious from the code

## Step 5: Create the commit

Use a HEREDOC for the message:

```
git commit -m "$(cat <<'EOF'
Subject line here

Body explaining why, not what.
EOF
)"
```

## Step 6: Verify

Run `git status` after committing to confirm success.
