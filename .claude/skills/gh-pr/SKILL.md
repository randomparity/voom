---
name: gh-pr
description: >
  Full-lifecycle pull request management for Rust/Cargo workspaces using the
  GitHub CLI. Use this skill whenever the user wants to create, update, review,
  fix up, or land a pull request. Triggers include: "create a PR", "open a PR",
  "push and PR", "ship it", "submit this for review", "update the PR",
  "address review feedback", "fix the review comments", "rebase and push",
  "squash my commits", "review this PR", "check out PR #N", "look at the open
  PRs", "what needs review", "merge the PR", "land it", or any request that
  involves interacting with GitHub pull requests from the command line. Also
  trigger when the user says "prep this for merge", "clean up the branch",
  "respond to CI failures", or asks to manage draft PRs. If the request touches
  `gh pr`, branch management around PRs, or any part of the code review cycle,
  use this skill.
---

# Pull Request Management

A comprehensive skill for the full PR lifecycle in Rust/Cargo workspaces:
create, update, review, and land pull requests — all through `gh` CLI and Git.

Every operation starts from the assumption that the workspace should be
**green** (tests pass, clippy clean, formatted) before anything touches GitHub.
When it isn't, fix it first.

---

## Prerequisites

Before any PR operation, verify:

1. `gh auth status` — CLI is authenticated
2. `git rev-parse --show-toplevel` — inside a Git repo
3. `cargo locate-project` — Cargo workspace present

If any check fails, tell the user what's missing and stop.

---

## Create PR

Use when the user has changes on a branch and wants to open a new pull request.

### Step 1 — Validate the workspace

```bash
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

If any command fails, fix the issue before proceeding. For formatting, run
`cargo fmt --all` and stage the result. For clippy or test failures, fix the
code. Don't open a PR against a red workspace.

### Step 2 — Prepare the commit(s)

Review what's changed:

```bash
git diff --stat
git diff --cached --stat
```

Stage only the files relevant to this PR — avoid sweeping `git add .` that
pulls in unrelated changes. Use targeted adds:

```bash
git add <file1> <file2> ...
```

If the changes logically break into multiple commits (e.g., a refactor
followed by a feature), create separate commits. Each commit message follows
conventional format:

```
<type>(<scope>): <summary>

<optional body — what and why, wrapped at 72 chars>
```

Types: `fix`, `feat`, `refactor`, `test`, `docs`, `chore`, `perf`, `ci`.
Scope is the crate or module name when it's not obvious from context.

If all changes are one logical unit, a single commit is fine.

### Step 3 — Push and create the PR

```bash
git push -u origin HEAD
```

Build the PR body in a temp file, then create:

```bash
gh pr create --title "<type>(<scope>): <summary>" --body-file /tmp/pr-body.md
```

**PR body structure:**

```markdown
## Summary

<1-2 sentence overview of what this PR does and why>

## Changes

- <bullet per meaningful change, grouped by crate if multi-crate>

## Testing

- <what tests were added or modified>
- <how to manually verify, if applicable>

## Notes

- <anything the reviewer should pay attention to>
- <trade-offs, follow-up work, related issues>
```

Reference issues with `closes #N`, `fixes #N`, or `relates to #N` as
appropriate in the summary.

### Draft PRs

If the user says "draft", "WIP", or "not ready for review", add `--draft`:

```bash
gh pr create --draft --title "..." --body-file /tmp/pr-body.md
```

---

## Update PR

Use when the user needs to push new changes to an existing PR — whether
addressing review feedback, fixing CI, or adding follow-up work.

### Addressing review comments

First, fetch the review comments:

```bash
gh pr view <NUMBER> --json reviews,comments --jq '.reviews[].body, .comments[].body'
```

For detailed review threads (inline code comments):

```bash
gh api repos/{owner}/{repo}/pulls/<NUMBER>/comments --jq '.[] | "\(.path):\(.line) — \(.body)"'
```

Work through each comment:
1. Read and understand the feedback
2. Make the fix or, if you disagree, prepare a response explaining why
3. Validate after each change: `cargo test --workspace && cargo clippy --workspace -- -D warnings`

### Pushing updates

After all feedback is addressed:

```bash
cargo fmt --all
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

Then commit and push. Prefer **fixup commits** during review so reviewers can
see what changed:

```bash
git add <changed-files>
git commit -m "fixup: address review — <brief description>"
git push
```

Add a PR comment summarizing what was addressed:

```bash
gh pr comment <NUMBER> --body "Addressed review feedback:
- <what was changed per comment>
- <any comments you pushed back on and why>"
```

### Rebasing on main

If the PR is behind the base branch:

```bash
git fetch origin main
git rebase origin/main
```

If there are conflicts, resolve them, then:

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
git push --force-with-lease
```

Always use `--force-with-lease` (not `--force`) to avoid overwriting someone
else's pushes.

---

## Review PR

Use when the user asks to look at, review, or check out someone else's PR.

### Check out the PR locally

```bash
gh pr checkout <NUMBER>
```

### Understand the changes

Start with the high-level picture:

```bash
gh pr diff <NUMBER> --stat
```

Then read the full diff, focusing on:
- **Correctness**: does the logic do what the PR claims?
- **Tests**: are new behaviors tested? Are edge cases covered?
- **Style**: does it follow existing repo conventions?
- **Safety**: error handling, unwrap usage, unsafe blocks, panic paths
- **Performance**: unnecessary allocations, O(n²) where O(n) would work
- **API surface**: are new public types/functions intentional and well-documented?

### Validate locally

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

### Submit the review

Provide a summary of findings. If the user wants to approve or request changes
via the CLI:

```bash
# Approve
gh pr review <NUMBER> --approve --body "Looks good — <brief note>"

# Request changes
gh pr review <NUMBER> --request-changes --body "<summary of issues>"

# Comment only (no approval/rejection)
gh pr review <NUMBER> --comment --body "<observations>"
```

For inline comments on specific lines, use:

```bash
gh api repos/{owner}/{repo}/pulls/<NUMBER>/comments \
  -f body="<comment>" \
  -f commit_id="$(gh pr view <NUMBER> --json headRefOid --jq .headRefOid)" \
  -f path="<file>" \
  -F line=<line_number> \
  -f side="RIGHT"
```

---

## Prep for Merge

Use when the PR is approved and the user wants to get it ready to land.

### Squash fixup commits

If the branch has fixup commits from the review cycle, clean them up:

```bash
git rebase -i --autosquash origin/main
```

This collapses any `fixup!` or `squash!` commits into their parents. The
result should be a clean commit history where each commit is a logical unit.

### Final validation

```bash
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

### Rebase on latest main

```bash
git fetch origin main
git rebase origin/main
git push --force-with-lease
```

### Mark ready for review (if draft)

```bash
gh pr ready <NUMBER>
```

---

## Merge PR

Use when the user wants to land the PR.

Check merge requirements first:

```bash
gh pr checks <NUMBER>
gh pr view <NUMBER> --json reviewDecision,mergeable,mergeStateStatus
```

If CI is green and reviews are approved:

```bash
# Merge commit (default — preserves full history)
gh pr merge <NUMBER> --merge

# Squash (single commit on main)
gh pr merge <NUMBER> --squash

# Rebase (linear history, preserves individual commits)
gh pr merge <NUMBER> --rebase
```

Follow the repo's convention. If you're unsure, check what recent merges
used:

```bash
git log --oneline --merges -5 main
```

If that shows merge commits, use `--merge`. If the history is linear, use
`--rebase`. If the user has a preference, follow it.

After merging, clean up:

```bash
gh pr view <NUMBER> --json headRefName --jq .headRefName | xargs git branch -d
git checkout main
git pull origin main
```

---

## Triage Open PRs

Use when the user asks what's open, what needs review, or wants a status
overview.

```bash
# All open PRs
gh pr list

# PRs needing your review
gh pr list --search "review-requested:@me"

# Your open PRs
gh pr list --author @me

# PRs with failing CI
gh pr list --json number,title,statusCheckRollup --jq '.[] | select(.statusCheckRollup | any(.conclusion == "FAILURE")) | "#\(.number) \(.title)"'
```

Present results as a concise table with PR number, title, author, CI status,
and review state.

---

## Error Recovery

- **CI failure after push**: fetch the CI logs with `gh run list` and
  `gh run view <ID> --log-failed`, diagnose, fix, push again.
- **Merge conflict during rebase**: resolve conflicts file by file, run full
  validation after, then `git rebase --continue` and `git push --force-with-lease`.
- **Accidentally pushed to wrong branch**: `git push origin --delete <branch>`,
  create the correct branch, push again.
- **PR opened against wrong base**: `gh pr edit <NUMBER> --base <correct-branch>`.
