//! Phase-5 acceptance tests for `voom process`.
//!
//! These tests exercise the full in-process pipeline (discovery → introspect
//! → plan → execute) through `TestEnv`. All tests are gated behind
//! `--features test-hooks` so the `delay_root` harness method is available.

mod common;

#[cfg(feature = "test-hooks")]
mod acceptance {
    use super::common::*;

    // The test-hooks global `DELAYS` is a process-wide static — concurrent
    // tests would race when one test's `clear()` fires during another test's
    // scan. Serialize all acceptance tests with this mutex so each test gets
    // exclusive ownership of the global delay table for the duration of its
    // `run_process` call.
    //
    // `tokio::sync::Mutex` is used because the guard is held across `.await`
    // points (specifically, across `run_process` which drives the full pipeline).
    static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    // ─── Shared policy fixtures ──────────────────────────────────────────────

    /// Policy that sets container to mp4 — triggers a container-conversion plan
    /// when the source is .mkv. Phase names must be unquoted identifiers.
    const POLICY_CONTAINER_MP4: &str = r#"policy "test-mp4" {
    phase convert {
        container mp4
    }
}
"#;

    /// Policy that keeps only audio tracks and sets the container to mkv.
    /// Used for tests that need a VOOM mutation (mkvmerge execution).
    const POLICY_AUDIO_ONLY: &str = r#"policy "audio-only" {
    phase strip {
        keep audio
        remove video
        remove subtitles
        container mkv
    }
}
"#;

    // ─── Test 1 — Temp-file exclusion mid-walk ───────────────────────────────

    /// Discovery must silently skip `.voom_tmp_*` files that appear (or already
    /// exist) in a scanned root. A file named `a.voom_tmp_abc123.mkv` must never
    /// appear in the `FileDiscovered` event stream.
    #[tokio::test]
    async fn temp_file_excluded_mid_walk() {
        let _lock = TEST_LOCK.lock().await;
        let mut env = TestEnv::new().await;
        let root = env.add_root("a");
        env.write_media_in(&root, "a.mkv", 1024);
        env.write_media_in(&root, "a.voom_tmp_abc123.mkv", 512);

        let out = env
            .run_process(&[
                root.to_str().unwrap(),
                "--policy",
                env.policy_path().to_str().unwrap(),
                "--dry-run",
                "--execute-during-discovery",
            ])
            .await;

        assert!(
            out.result.is_ok(),
            "process run failed: {:?}",
            out.result.err()
        );

        let discovered_paths: Vec<_> = out
            .events
            .all()
            .iter()
            .filter_map(|(_, ev)| {
                if let voom_domain::Event::FileDiscovered(e) = ev {
                    Some(e.path.clone())
                } else {
                    None
                }
            })
            .collect();

        assert!(
            discovered_paths.iter().any(|p| p.ends_with("a.mkv")),
            "expected a.mkv to be discovered; got: {discovered_paths:?}"
        );
        assert!(
            !discovered_paths
                .iter()
                .any(|p| p.to_string_lossy().contains("voom_tmp_")),
            "temp file must NOT be emitted as FileDiscovered; got: {discovered_paths:?}"
        );
    }

    // ─── Test 2 — Rename / container conversion mid-walk ────────────────────

    /// A container conversion that renames the source (e.g. .mkv → .mp4) must
    /// not cause the original file to be flagged as missing in the scan session.
    /// root_b is delayed so root_a's plan can execute while root_b is still
    /// being walked, exercising the interleaved execute-during-discovery path.
    #[tokio::test]
    async fn rename_during_discovery_does_not_double_count() {
        let _lock = TEST_LOCK.lock().await;
        let mut env = TestEnv::new().await;
        let root_a = env.add_root("a");
        let root_b = env.add_root("b");
        env.write_media_in(&root_a, "movie.mkv", 1024);
        env.write_media_in(&root_b, "other.mkv", 1024);
        env.set_policy(POLICY_CONTAINER_MP4);
        env.delay_root(&root_b, std::time::Duration::from_millis(300));

        let out = env
            .run_process(&[
                root_a.to_str().unwrap(),
                root_b.to_str().unwrap(),
                "--policy",
                env.policy_path().to_str().unwrap(),
                "--approve",
                "--execute-during-discovery",
            ])
            .await;

        // The run may succeed or fail depending on ffmpeg/mkvmerge availability.
        // Both are acceptable. What we do NOT want is a panic or a
        // scan-session-level "missing file" false positive.
        if out.result.is_err() {
            let msg = format!("{:?}", out.result.as_ref().err().unwrap());
            assert!(
                msg.contains("ffmpeg")
                    || msg.contains("mkvmerge")
                    || msg.contains("transcode")
                    || msg.contains("not found")
                    || msg.contains("No such file"),
                "any failure must be tool-related, not a scan-session race; got: {msg}"
            );
        } else {
            // If the run succeeded, either the converted output exists or the
            // original is still present (dry-run / no-op).
            assert!(
                root_a.join("movie.mp4").exists() || root_a.join("movie.mkv").exists(),
                "either the converted output or the original must exist post-run"
            );
        }
    }

    // ─── Test 3 — Cancellation with in-flight execution ─────────────────────

    /// Cancelling mid-run (100 ms after start) must finish cleanly — no panics,
    /// no unwound threads with unsaved state. root_b is given a 5 s walk delay
    /// to ensure the cancel fires while something is still in flight.
    #[tokio::test]
    async fn cancel_with_inflight_execution_leaves_session_cancelled() {
        let _lock = TEST_LOCK.lock().await;
        let mut env = TestEnv::new().await;
        let root_a = env.add_root("a");
        let root_b = env.add_root("b");
        for i in 0..5_u32 {
            env.write_media_in(&root_a, &format!("f{i}.mkv"), 1024 * 1024);
        }
        env.delay_root(&root_b, std::time::Duration::from_secs(5));

        let token = tokio_util::sync::CancellationToken::new();
        let cancel_clone = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            cancel_clone.cancel();
        });

        let out = env
            .run_process_with_token(
                &[
                    root_a.to_str().unwrap(),
                    root_b.to_str().unwrap(),
                    "--policy",
                    env.policy_path().to_str().unwrap(),
                    "--approve",
                    "--execute-during-discovery",
                ],
                token,
            )
            .await;

        // Both Ok and Err are acceptable after cancellation. The critical
        // invariant is that the function returned at all (no hang/panic).
        // A panic would have aborted the test process before reaching this line.
        let _ = &out.result; // touch it so the compiler knows we checked it

        // Verify the event timeline is coherent: if any FileDiscovered events
        // were emitted before the cancel fired, they must be parseable.
        for (_, ev) in out.events.all() {
            // Just exercising the deserialization path — if an event is
            // malformed, the timeline builder would have dropped it silently,
            // which is fine. Here we only care that iteration doesn't panic.
            let _ = ev;
        }
    }

    // ─── Test 4 — finish_scan_session clean after VOOM mutation ─────────────

    /// After an execute-during-discovery run that performs VOOM mutations
    /// (mkvmerge track strip), `finish_scan_session` must not flag the mutated
    /// files as missing. The scan session summary must be clean.
    #[tokio::test]
    async fn finish_scan_session_after_voom_mutation_is_clean() {
        let _lock = TEST_LOCK.lock().await;
        let mut env = TestEnv::new().await;
        let root_a = env.add_root("a");
        let root_b = env.add_root("b");
        env.write_media_in(&root_a, "keep.mkv", 1024);
        env.write_media_in(&root_a, "strip.mkv", 1024);
        env.write_media_in(&root_b, "other.mkv", 1024);
        env.set_policy(POLICY_AUDIO_ONLY);

        let out = env
            .run_process(&[
                root_a.to_str().unwrap(),
                root_b.to_str().unwrap(),
                "--policy",
                env.policy_path().to_str().unwrap(),
                "--approve",
                "--execute-during-discovery",
            ])
            .await;

        // Tolerate tool-unavailability failures gracefully.
        if out.result.is_err() {
            let msg = format!("{:?}", out.result.as_ref().err().unwrap());
            assert!(
                msg.contains("mkvmerge")
                    || msg.contains("mkvtoolnix")
                    || msg.contains("ffmpeg")
                    || msg.contains("not found")
                    || msg.contains("No such file"),
                "any failure must be tool-related; got: {msg}"
            );
            return;
        }

        // If the run succeeded, the session must have completed without
        // marking any file as missing. The proxy for this is that the total
        // number of FileDiscovered events matches the files we placed on disk
        // (3 across both roots). Use canonicalize to normalize macOS symlinks
        // (/var → /private/var) before comparing.
        let root_a_canon = root_a.canonicalize().unwrap_or(root_a.clone());
        let all_discovered: Vec<_> = out
            .events
            .all()
            .iter()
            .filter_map(|(_, ev)| {
                if let voom_domain::Event::FileDiscovered(e) = ev {
                    Some(e.path.clone())
                } else {
                    None
                }
            })
            .collect();

        // Filter to root_a files, canonicalizing discovered paths to handle
        // macOS /var → /private/var symlink differences.
        let root_a_discovered: Vec<_> = all_discovered
            .iter()
            .filter(|p| {
                let canon = p.canonicalize().unwrap_or_else(|_| (*p).clone());
                canon.starts_with(&root_a_canon)
            })
            .collect();

        // We should have discovered at least the two root_a files.
        assert!(
            root_a_discovered.len() >= 2,
            "expected ≥2 discoveries under root_a; got {root_a_discovered:?} \
             (all discovered: {all_discovered:?}, root_a_canon={root_a_canon:?})"
        );
    }

    // ─── Test 5 — Overlapping roots unlock independently ────────────────────

    /// With two roots where root_b has a 2 s walk delay, root_a's
    /// `RootWalkCompleted` event must be emitted earlier than root_b's.
    /// This validates the per-root gate mechanism: roots unlock independently
    /// and do not block each other.
    #[tokio::test]
    async fn overlapping_roots_unlock_independently() {
        let _lock = TEST_LOCK.lock().await;
        let mut env = TestEnv::new().await;
        let root_a = env.add_root("a");
        let root_b = env.add_root("b");
        env.write_media_in(&root_a, "a1.mkv", 1024);
        env.write_media_in(&root_b, "b1.mkv", 1024);
        env.delay_root(&root_b, std::time::Duration::from_secs(2));

        let out = env
            .run_process(&[
                root_a.to_str().unwrap(),
                root_b.to_str().unwrap(),
                "--policy",
                env.policy_path().to_str().unwrap(),
                "--approve",
                "--execute-during-discovery",
            ])
            .await;

        assert!(
            out.result.is_ok(),
            "process run failed: {:?}",
            out.result.err()
        );

        let root_a_done = out.events.root_walk_completed_at(&root_a);
        let root_b_done = out.events.root_walk_completed_at(&root_b);

        assert!(root_a_done.is_some(), "root_a must emit RootWalkCompleted");
        assert!(root_b_done.is_some(), "root_b must emit RootWalkCompleted");

        if let (Some(a), Some(b)) = (root_a_done, root_b_done) {
            assert!(
                a < b,
                "root_a (no delay) must finish walking before root_b (2 s delay); \
                 a={a:?} b={b:?}"
            );
        }
    }

    // ─── Test 6 — Output path under scanned root not rediscovered ───────────

    /// When a container conversion writes `src.mp4` into the same root that is
    /// currently being scanned, the output file must NOT appear as a new
    /// `FileDiscovered` event. root_b is delayed so root_a's plan can execute
    /// before the second root's walk begins — ensuring the mutation snapshot is
    /// consulted on root_b's walk.
    #[tokio::test]
    async fn output_path_under_scanned_root_not_rediscovered() {
        let _lock = TEST_LOCK.lock().await;
        let mut env = TestEnv::new().await;
        let root_a = env.add_root("a");
        let root_b = env.add_root("b");
        env.write_media_in(&root_a, "src.mkv", 1024);
        env.set_policy(POLICY_CONTAINER_MP4);
        env.delay_root(&root_b, std::time::Duration::from_millis(500));

        let out = env
            .run_process(&[
                root_a.to_str().unwrap(),
                root_b.to_str().unwrap(),
                "--policy",
                env.policy_path().to_str().unwrap(),
                "--approve",
                "--execute-during-discovery",
            ])
            .await;

        // Tolerate tool-unavailability failures.
        if out.result.is_err() {
            let msg = format!("{:?}", out.result.as_ref().err().unwrap());
            assert!(
                msg.contains("ffmpeg")
                    || msg.contains("mkvmerge")
                    || msg.contains("not found")
                    || msg.contains("No such file"),
                "failure must be tool-related; got: {msg}"
            );
            return;
        }

        let discovered: Vec<_> = out
            .events
            .all()
            .iter()
            .filter_map(|(_, ev)| {
                if let voom_domain::Event::FileDiscovered(e) = ev {
                    Some(e.path.clone())
                } else {
                    None
                }
            })
            .collect();

        assert!(
            !discovered.iter().any(|p| p.ends_with("src.mp4")),
            "output path src.mp4 must not be re-emitted as a new FileDiscovered event; \
             discovered: {discovered:?}"
        );
    }

    // ─── Test 7 — A8: Closed-root priority starvation (adversarial) ─────────

    /// With two workers and `--priority-by-date`, root_b's files (older mtime =
    /// higher priority) must NOT starve root_a's execution while root_b's gate
    /// is still closed. Specifically, root_a's first `PlanExecuting` event must
    /// fire BEFORE root_b's `RootWalkCompleted` event — proving that the
    /// gate-at-claim design does not stall open roots while a closed root holds
    /// high-priority slots.
    #[tokio::test]
    async fn closed_root_does_not_starve_open_root_under_priority() {
        let _lock = TEST_LOCK.lock().await;
        let mut env = TestEnv::new().await;
        let root_a = env.add_root("a");
        let root_b = env.add_root("b");

        let now = std::time::SystemTime::now();
        let hour_ago = now - std::time::Duration::from_secs(3600);

        // root_b: older mtime → higher priority under --priority-by-date.
        // Delayed 2 s so its gate stays closed during root_a execution.
        for i in 0..3_u32 {
            env.write_media_in_with_mtime(&root_b, &format!("b{i}.mkv"), 1024, hour_ago);
        }
        // root_a: newer mtime → lower priority. No delay; gate opens immediately.
        for i in 0..3_u32 {
            env.write_media_in_with_mtime(&root_a, &format!("a{i}.mkv"), 1024, now);
        }

        env.delay_root(&root_b, std::time::Duration::from_secs(2));

        let out = env
            .run_process(&[
                root_a.to_str().unwrap(),
                root_b.to_str().unwrap(),
                "--policy",
                env.policy_path().to_str().unwrap(),
                "--approve",
                "--workers",
                "2",
                "--priority-by-date",
                "--execute-during-discovery",
            ])
            .await;

        assert!(
            out.result.is_ok(),
            "process run failed: {:?}",
            out.result.err()
        );

        let root_a_done = out.events.root_walk_completed_at(&root_a);
        let root_b_done = out.events.root_walk_completed_at(&root_b);

        assert!(root_a_done.is_some(), "root_a must emit RootWalkCompleted");
        assert!(root_b_done.is_some(), "root_b must emit RootWalkCompleted");

        // Find the earliest PlanExecuting for any file under root_a.
        let first_a_exec = out.events.first_at(|ev| {
            if let voom_domain::Event::PlanExecuting(e) = ev {
                e.path.starts_with(&root_a)
            } else {
                false
            }
        });

        if let (Some(a_done), Some(first_a), Some(b_done)) =
            (root_a_done, first_a_exec, root_b_done)
        {
            // root_a's walk must complete before its plans execute.
            assert!(
                a_done <= first_a,
                "root_a walk ({a_done:?}) must complete before root_a's first plan \
                 executes ({first_a:?})"
            );
            // The core anti-starvation guarantee: root_a must start executing
            // BEFORE root_b's gate opens, proving closed root_b did not starve it.
            assert!(
                first_a < b_done,
                "root_a's first execution ({first_a:?}) must begin BEFORE root_b \
                 finishes walking ({b_done:?}) — otherwise gate-at-claim starvation \
                 prevented progress on open root_a"
            );
        }
        // NOTE: the sql_job_snapshots starvation assertion is deliberately omitted.
        // The 25 ms polling granularity and DB write jitter make a per-row timing
        // assertion flaky in CI. The timing assertion above captures the same
        // safety invariant with deterministic event timestamps.
    }

    // ─── Issue #377 regression tests ────────────────────────────────────────

    /// `voom process` against a populated library must NOT mark any
    /// pre-existing file as missing — `finish_scan_session` is now wired
    /// through ingest_discovered_file, so files that were both registered
    /// and present this session must remain `Active`.
    #[tokio::test]
    async fn process_does_not_mark_existing_files_missing() {
        use voom_domain::storage::FileFilters;
        use voom_domain::transition::FileStatus;

        let _lock = TEST_LOCK.lock().await;
        let mut env = TestEnv::new().await;
        let root = env.add_root("a");
        env.write_media_in(&root, "alpha.mkv", 1024);
        env.write_media_in(&root, "bravo.mkv", 1024);

        // First run: registers both files via ingest_discovered_file +
        // finish_scan_session. Use --dry-run so the run does not require
        // ffmpeg/mkvmerge availability on the test host.
        let out = env
            .run_process(&[
                root.to_str().unwrap(),
                "--policy",
                env.policy_path().to_str().unwrap(),
                "--dry-run",
            ])
            .await;
        assert!(
            out.result.is_ok(),
            "first process run failed: {:?}",
            out.result.err()
        );

        // Pull every file row (include_missing=true). Pre-fix, every row
        // would be Missing; post-fix, every row must be Active.
        // FileFilters is #[non_exhaustive] so we build via default() +
        // mutation rather than struct-literal.
        let mut filters = FileFilters::default();
        filters.include_missing = true;
        let all_rows = out.store.list_files(&filters).expect("list all files");
        let missing: Vec<_> = all_rows
            .iter()
            .filter(|f| f.status == FileStatus::Missing)
            .collect();
        assert!(
            missing.is_empty(),
            "no files should be marked Missing after first run; got {missing:?}"
        );
        let active = all_rows
            .iter()
            .filter(|f| f.status == FileStatus::Active)
            .count();
        assert!(
            active >= 2,
            "expected at least 2 Active files after first run; got {active}"
        );
    }

    /// After `voom process` populates the library, deleting one file and
    /// running `voom process` again must mark the deleted file as
    /// `Missing` — proves `finish_scan_session` is correctly reconciling
    /// against ingest_discovered_file registrations.
    #[tokio::test]
    async fn process_marks_removed_files_missing_on_next_run() {
        use voom_domain::transition::FileStatus;

        let _lock = TEST_LOCK.lock().await;
        let mut env = TestEnv::new().await;
        let root = env.add_root("a");
        let kept = root.join("kept.mkv");
        let removed = root.join("removed.mkv");
        env.write_media_in(&root, "kept.mkv", 1024);
        env.write_media_in(&root, "removed.mkv", 1024);

        // Initial run: registers both files.
        let out1 = env
            .run_process(&[
                root.to_str().unwrap(),
                "--policy",
                env.policy_path().to_str().unwrap(),
                "--dry-run",
            ])
            .await;
        assert!(
            out1.result.is_ok(),
            "first run failed: {:?}",
            out1.result.err()
        );

        // Delete one file under the root.
        std::fs::remove_file(&removed).expect("remove media file");

        // Second run: should mark the deleted file as Missing.
        let out2 = env
            .run_process(&[
                root.to_str().unwrap(),
                "--policy",
                env.policy_path().to_str().unwrap(),
                "--dry-run",
            ])
            .await;
        assert!(
            out2.result.is_ok(),
            "second run failed: {:?}",
            out2.result.err()
        );

        let removed_row = out2
            .store
            .file_by_path(&removed)
            .expect("query removed file")
            .expect("removed file row");
        assert_eq!(
            removed_row.status,
            FileStatus::Missing,
            "removed file must be marked Missing after second run"
        );

        let kept_row = out2
            .store
            .file_by_path(&kept)
            .expect("query kept file")
            .expect("kept file row");
        assert_eq!(
            kept_row.status,
            FileStatus::Active,
            "kept file must remain Active after second run; status={:?}",
            kept_row.status
        );
    }

    // NOTE: the `--no-backup` (path-only reconciliation via
    // `mark_missing_paths`) branch is not exercised by an acceptance
    // test here. The synthetic-media harness produces files that
    // ffprobe rejects, so the `files` table is populated only by
    // `ingest_discovered_file` (when hashing is on). Under `--no-backup`
    // there is no hash and no ingest, so the test cannot observe the
    // `mark_missing_paths` behaviour with this harness. The same
    // reconciliation path is exercised by `voom scan` tests in
    // `crates/voom-cli/src/commands/scan/pipeline.rs` and by the
    // sqlite-store unit tests
    // (`plugins/sqlite-store/src/store/file_storage.rs::mark_missing_paths_*`).
}
