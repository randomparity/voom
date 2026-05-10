# Issue 207 Remote Backups Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add rclone-backed remote backup destinations that upload and verify originals before destructive processing.

**Architecture:** Extend `voom-backup-manager` with typed destination config and a small destination runner abstraction. Keep the existing local `.vbak` backup as the staging and restore path, then upload the source bytes to each configured remote before returning from `PlanExecuting`.

**Tech Stack:** Rust 2021, native VOOM plugin config through `PluginContext::parse_config`, `std::process::Command` subprocess execution, existing CLI/docs/example policy infrastructure.

---

## File Map

- Modify: `plugins/backup-manager/src/lib.rs` for config parsing, record metadata, and event output.
- Modify: `plugins/backup-manager/src/backup.rs` for destination upload orchestration.
- Create: `plugins/backup-manager/src/destination.rs` for destination config, validation, path layout, and rclone command runner.
- Modify: `plugins/backup-manager/Cargo.toml` only if serde derive support is missing.
- Modify: `crates/voom-cli/src/app.rs` only if plugin init needs a constructed config.
- Modify: `docs/remote-backups.md` for user documentation.
- Modify: `docs/cli-reference.md` to link backup remote configuration.
- Create: `docs/examples/remote-backup-transcode.voom`.
- Create: `docs/examples/remote-backup-rclone.toml`.
- Create: `docs/examples/remote-backup-s3.toml`.
- Modify: `docs/examples/README.md` to list the new example.
- Create: `docs/functional-test-plan-remote-backups.md`.
- Create: `docs/plans/issue-207-adversarial-review.md`.

### Task 1: Destination Config And Validation

**Files:**
- Create: `plugins/backup-manager/src/destination.rs`
- Modify: `plugins/backup-manager/src/lib.rs`

- [ ] **Step 1: Write failing config tests**

Add tests in `plugins/backup-manager/src/destination.rs`:

```rust
#[test]
fn rejects_duplicate_destination_names() {
    let config = BackupDestinationsConfig {
        destinations: vec![
            BackupDestinationConfig::rclone("offsite", "b2:voom"),
            BackupDestinationConfig::rclone("offsite", "s3:voom"),
        ],
        ..BackupDestinationsConfig::default()
    };

    let err = config.validate().unwrap_err();

    assert!(err.to_string().contains("duplicate backup destination"));
}

#[test]
fn rejects_rclone_destination_without_remote() {
    let config = BackupDestinationsConfig {
        destinations: vec![BackupDestinationConfig {
            name: "offsite".to_string(),
            kind: DestinationKind::Rclone,
            remote: None,
            bandwidth_limit: None,
        }],
        ..BackupDestinationsConfig::default()
    };

    let err = config.validate().unwrap_err();

    assert!(err.to_string().contains("requires remote"));
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```sh
cargo test -p voom-backup-manager destination::tests::rejects_
```

Expected: fail because `destination` module and types do not exist.

- [ ] **Step 3: Implement destination config**

Create `destination.rs` with `DestinationKind`, `BackupDestinationConfig`,
`BackupDestinationsConfig`, validation, and helper constructors for tests.
Expose it from `lib.rs` with `pub mod destination;`.

- [ ] **Step 4: Run tests to verify GREEN**

Run:

```sh
cargo test -p voom-backup-manager destination::tests
```

Expected: all destination config tests pass.

- [ ] **Step 5: Commit**

```sh
git add plugins/backup-manager/src/lib.rs plugins/backup-manager/src/destination.rs
git commit -m "feat(backup): add remote destination config"
```

### Task 2: Upload Orchestration And Rclone Runner

**Files:**
- Modify: `plugins/backup-manager/src/backup.rs`
- Modify: `plugins/backup-manager/src/destination.rs`
- Modify: `plugins/backup-manager/src/lib.rs`

- [ ] **Step 1: Write failing upload tests**

Add tests that construct fake destination runners:

```rust
#[test]
fn backup_file_uploads_to_all_remote_destinations() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("movie.mkv");
    fs::write(&source, b"movie").unwrap();
    let mut uploads = Vec::new();
    let config = remote_test_config(dir.path());

    let record = backup_file_with_destinations(
        &config,
        &source,
        |_backup, _source| Ok(()),
        |request| {
            uploads.push((request.destination_name.to_string(), request.remote_path.clone()));
            Ok(RemoteUploadReceipt { verified: true })
        },
    )
    .unwrap();

    assert_eq!(record.remote_backups.len(), 2);
    assert_eq!(uploads.len(), 2);
}

#[test]
fn remote_upload_failure_blocks_backup() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("movie.mkv");
    fs::write(&source, b"movie").unwrap();
    let config = remote_test_config(dir.path());

    let err = backup_file_with_destinations(
        &config,
        &source,
        |_backup, _source| Ok(()),
        |_request| Err(plugin_err("offsite upload failed")),
    )
    .unwrap_err();

    assert!(err.to_string().contains("offsite upload failed"));
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```sh
cargo test -p voom-backup-manager backup::tests::backup_file_uploads_to_all_remote_destinations backup::tests::remote_upload_failure_blocks_backup
```

Expected: fail because upload helpers do not exist.

- [ ] **Step 3: Implement upload orchestration**

Add `RemoteBackupRecord`, `RemoteUploadRequest`, `RemoteUploadReceipt`, and
`backup_file_with_destinations`. Keep `backup_file` as a wrapper that passes
the real rclone runner.

- [ ] **Step 4: Add rclone command tests**

Add tests in `destination.rs` for argv generation:

```rust
#[test]
fn rclone_copyto_uses_argv_without_shell() {
    let command = RcloneCommand::copyto(
        "rclone",
        Path::new("/tmp/movie.mkv"),
        "b2:voom/backup.vbak",
        Some("10M"),
    );

    assert_eq!(command.program, "rclone");
    assert_eq!(command.args[0], "copyto");
    assert!(command.args.contains(&"--bwlimit".to_string()));
}
```

- [ ] **Step 5: Run focused tests**

Run:

```sh
cargo test -p voom-backup-manager
```

Expected: all backup-manager tests pass.

- [ ] **Step 6: Commit**

```sh
git add plugins/backup-manager/src/lib.rs plugins/backup-manager/src/backup.rs plugins/backup-manager/src/destination.rs
git commit -m "feat(backup): upload backups to rclone destinations"
```

### Task 3: Plugin Init And Event Metadata

**Files:**
- Modify: `plugins/backup-manager/src/lib.rs`

- [ ] **Step 1: Write failing init tests**

Add tests that initialize `BackupManagerPlugin` with JSON config containing
destinations and verify `backup_file` uses the parsed config.

- [ ] **Step 2: Run tests to verify RED**

Run:

```sh
cargo test -p voom-backup-manager tests::test_init_parses_remote_destinations
```

Expected: fail because `init` does not parse config.

- [ ] **Step 3: Implement init parsing**

Derive `Deserialize` for backup config types, implement `Plugin::init`, validate
destinations, and store parsed config on the plugin before registration.

- [ ] **Step 4: Include remote metadata in event result**

Update `PlanExecuting` result JSON to include destination names and paths, with
no credential values.

- [ ] **Step 5: Run tests**

Run:

```sh
cargo test -p voom-backup-manager
```

Expected: all backup-manager tests pass.

- [ ] **Step 6: Commit**

```sh
git add plugins/backup-manager/src/lib.rs plugins/backup-manager/src/destination.rs
git commit -m "feat(backup): load remote destinations from config"
```

### Task 4: Docs, Examples, And Functional Test Plan

**Files:**
- Create: `docs/remote-backups.md`
- Modify: `docs/cli-reference.md`
- Create: `docs/examples/remote-backup-transcode.voom`
- Create: `docs/examples/remote-backup-rclone.toml`
- Create: `docs/examples/remote-backup-s3.toml`
- Modify: `docs/examples/README.md`
- Create: `docs/functional-test-plan-remote-backups.md`

- [ ] **Step 1: Write docs and examples**

Document config keys, rclone prerequisites, fake-rclone testing, failure
behavior, credential handling, and current restore limitations.

- [ ] **Step 2: Include generated-corpus functional plan**

The test plan must use:

```sh
scripts/generate-test-corpus /tmp/voom-remote-backup-corpus --profile smoke --duration 1
VOOM_CONFIG_HOME=/tmp/voom-remote-config cargo run -p voom-cli -- process \
  /tmp/voom-remote-backup-corpus --policy docs/examples/remote-backup-transcode.voom --yes
```

It must configure fake rclone to copy into `/tmp/voom-remote-backup-target`.

- [ ] **Step 3: Verify examples parse**

Run:

```sh
cargo test -p voom-dsl --test parser_snapshots example_remote_backup_transcode_parses_and_validates
```

Expected: the remote backup example parses and validates.

- [ ] **Step 4: Commit**

```sh
git add docs/remote-backups.md docs/cli-reference.md docs/examples/remote-backup-transcode.voom docs/examples/remote-backup-rclone.toml docs/examples/remote-backup-s3.toml docs/examples/README.md docs/functional-test-plan-remote-backups.md
git commit -m "docs(backup): document remote backup destinations"
```

### Task 5: Adversarial Review And Final Verification

**Files:**
- Create: `docs/plans/issue-207-adversarial-review.md`

- [ ] **Step 1: Write adversarial review**

Review the plan and resulting code against issue 207 acceptance criteria, with
explicit notes for what is rclone-backed, what is not native protocol support,
credential safety, failure blocking, and restore limitations.

- [ ] **Step 2: Run final verification**

Run:

```sh
cargo fmt --check
cargo test -p voom-backup-manager
cargo test -p voom-dsl --test parser_snapshots
cargo clippy -p voom-backup-manager --all-targets -- -D warnings
```

Expected: every command exits 0 with no warnings.

- [ ] **Step 3: Commit review artifact**

```sh
git add docs/plans/issue-207-adversarial-review.md
git commit -m "docs(backup): review remote backup implementation risks"
```

### Task 6: PR Process

- [ ] Push branch:

```sh
git push -u origin feat/issue-207-remote-backups
```

- [ ] Open PR:

```sh
gh pr create --fill --base main --head feat/issue-207-remote-backups
```

- [ ] Watch checks:

```sh
gh pr checks --watch
```

- [ ] Address review feedback with additional small conventional commits.

- [ ] Merge after approval and green checks:

```sh
gh pr merge --squash --delete-branch
```
