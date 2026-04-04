---
name: gh-issue
description: >
  Autonomous batch implementation of GitHub issues for Rust/Cargo workspaces.
  Use this skill whenever the user asks to implement, fix, or close one or more
  GitHub issues end-to-end — especially when they say things like "implement
  issues #X, #Y, #Z", "close out these tickets", "work through these GH issues",
  "pick up issues and ship a PR", or any request that involves fetching issue
  details from GitHub and turning them into working code with tests, commits,
  and a pull request. Also trigger when the user pastes a list of issue numbers
  and expects autonomous implementation with no further input. This skill covers
  the full lifecycle: plan → implement → validate → review → commit → PR.
---

# GitHub Issue Batch Implementation

An autonomous workflow for taking a set of GitHub issue numbers, understanding
each one, planning the work, implementing across a Rust/Cargo workspace,
validating, self-reviewing, and shipping a single PR that covers all issues.

The guiding principle is **zero-input autonomy**: make reasonable engineering
decisions and document your reasoning rather than asking the user to
choose. The user will review the PR — that's their feedback checkpoint.

---

## Prerequisites

Before starting, verify the environment:

1. Confirm `gh` CLI is authenticated: `gh auth status`
2. Confirm you're inside a Git repo: `git rev-parse --show-toplevel`
3. Confirm it's a Cargo workspace: check for a root `Cargo.toml` with
   `[workspace]` or at least one crate with `Cargo.toml`
4. Confirm the working tree is clean: `git status --porcelain` should be empty
   (if not, warn the user and stop)

If any prerequisite fails, tell the user what's missing and stop.

---

## Phase 1 — Fetch and Understand Issues

For each issue number the user provides:

```bash
gh issue view <NUMBER> --json title,body,labels,assignees,comments
```

Read the full issue body and comments. Extract:
- **Goal**: what the issue is asking for
- **Acceptance criteria**: any concrete requirements or test expectations
- **Relevant code areas**: files, modules, or crates mentioned

If an issue references other issues or PRs, fetch those too for context.

### Prioritize

Sort issues by dependency order — if issue A's changes are needed by issue B,
A goes first. When there are no dependencies, prefer this order:
1. Bug fixes (correctness matters before features)
2. Refactors that unblock other work
3. New features
4. Cleanups / nice-to-haves

If labels or the user's ordering suggest a different priority, follow that
instead. Document the chosen order and reasoning.

---

## Phase 2 — Explore and Plan

Create a feature branch:

```bash
git checkout -b impl/<short-slug>
```

Use a slug that captures the batch (e.g., `impl/issue-42-55-61` or
`impl/auth-refactor-batch`).

For **each** issue, in priority order:

1. **Explore the code.** Read the files and modules that the issue touches.
   Understand the existing patterns, error handling style, naming conventions,
   and test structure already in use. Check for related tests, doc comments,
   and public API surface.

2. **Write a brief implementation plan** — a few bullet points covering:
   - Which files/crates are affected
   - What changes are needed (new types, modified functions, new tests, etc.)
   - Any cross-cutting concerns (shared types, dependency changes, migration)
   - Risks or ambiguities and how you'll resolve them

Do **not** start writing code until all plans are ready. Seeing all plans
together lets you spot conflicts, shared refactors, and ordering issues before
you're deep in implementation.

---

## Phase 3 — Implement

Work through issues in priority order. For each issue:

1. **Make the changes.** Follow existing code conventions in the repo:
   - Match the existing formatting, naming, and module structure
   - Use the error handling pattern already established (thiserror, anyhow, 
     custom enums — whatever the project uses)
   - Add or update doc comments for any public API changes
   - Add or update tests to cover new behavior and edge cases

2. **Validate immediately** after finishing the issue's changes:

   ```bash
   cargo test --workspace
   cargo clippy --workspace -- -D warnings
   ```

   If either fails, fix the failures before moving to the next issue. Don't
   accumulate breakage — each issue should leave the workspace green.

3. **Stage the changes** for this issue (don't commit yet — that happens in
   Phase 5). Keep a mental note of which files belong to which issue.

### Cross-cutting changes

If multiple issues touch the same file or type, handle it this way:
- Shared refactors (e.g., extracting a common type) belong to whichever issue
  motivates them. Mention the shared nature in the commit message.
- If a later issue conflicts with an earlier one's changes, resolve in favor
  of the later issue's requirements and adjust the earlier code if needed.
  Re-run validation after any such adjustment.

---

## Phase 4 — Self-Review

After all issues are implemented and the workspace is green, do a dedicated
review pass across **all** changed files. Look for:

- **Dead code**: unused imports, functions, types, or variables introduced by
  the changes
- **Unnecessary complexity**: over-abstraction, premature generalization, or
  convoluted control flow that could be simplified
- **Missing edge cases**: what happens with empty inputs, zero-length
  collections, None/Err paths, integer overflow, concurrent access
- **Consistency**: do new names match existing conventions? Are error messages
  helpful? Do new public items have doc comments?
- **Test quality**: are tests actually asserting meaningful behavior, or just
  that code doesn't panic? Are there missing negative test cases?

Fix anything you find. Re-run validation:

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

---

## Phase 5 — Commit, Push, and PR

### Commits

Create **one commit per issue**. The commit message format:

```
<type>: <concise summary> (closes #<NUMBER>)

<optional body explaining the what and why, wrapped at 72 chars>
```

Where `<type>` is one of: `fix`, `feat`, `refactor`, `test`, `docs`, `chore`.

Use `git add -p` or targeted `git add <files>` to stage only the files
belonging to each issue. Commit in the same priority order you implemented.

If the self-review pass introduced changes that don't belong to any specific
issue, add a final commit:

```
chore: post-implementation cleanup and review fixes
```

### Push and PR

```bash
git push -u origin HEAD
```

Create the PR:

```bash
gh pr create --title "<concise title covering the batch>" --body-file /tmp/pr-body.md
```

The PR body (`/tmp/pr-body.md`) should contain:

1. **Summary table** — one row per issue:

   | Issue | Title | Type | Files Changed |
   |-------|-------|------|---------------|
   | #42 | Fix auth token refresh | fix | `crates/auth/src/token.rs`, `crates/auth/src/lib.rs` |
   | #55 | Add retry logic to HTTP client | feat | `crates/http/src/client.rs`, `crates/http/src/retry.rs` |

2. **Implementation notes** — per-issue sections with:
   - What was done and why (especially non-obvious decisions)
   - Any trade-offs or alternatives considered
   - Test coverage added

3. **Review notes** — anything the reviewer should pay extra attention to
   (risky changes, performance implications, API surface changes)

4. **Validation** — confirm that `cargo test --workspace` and
   `cargo clippy --workspace -- -D warnings` pass clean.

---

## Error Handling During the Workflow

- **Issue not found**: skip it, note the skip in the PR body, continue with
  the rest.
- **Ambiguous issue**: if an issue is genuinely unclear and you can't make a
  reasonable interpretation, skip it with explanation rather than guessing
  wildly. Mention it in the PR body.
- **Persistent test failure**: if you can't fix a test failure after 3
  attempts, revert that issue's changes, note it in the PR body, and continue
  with the remaining issues.
- **Merge conflicts with main**: if the branch is far behind, rebase before
  pushing. If rebase conflicts are complex, note them in the PR.
