# E2E Policy Audit Harness

Script-driven end-to-end test of VOOM applying any `.voom` policy to a media
library from a clean database. Captures rich pre/post state from both VOOM's
view (the SQLite DB) and ground truth (independent `ffprobe`), and emits diffs
the operator uses to judge policy correctness.

The harness is policy-agnostic: it does not parse `.voom` files and does not
encode any expected outcomes. Pipeline correctness (build, scan, jobs reach a
terminal state, no data loss, web endpoints up) is gated by the harness;
semantic correctness ("did the policy do what I wanted") is the operator's
judgment from the diffs.

## Usage

(Filled in by Task 15.)

## Pre-conditions

(Filled in by Task 15.)

## Run-dir layout

(Filled in by Task 15.)

See `docs/superpowers/specs/2026-05-05-e2e-policy-audit-design.md` for the
full design.
