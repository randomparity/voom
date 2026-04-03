# Lifecycle Tests Review Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Address three findings from the Codex adversarial review: enforce content-hash uniqueness in random corpus generation, make scale presets actually generate scaled corpora via `--count`, and add end-to-end tests that exercise `--count` and `--corrupt` together.

**Architecture:** Fix the Python corpus generator to track content signatures and guarantee uniqueness, add a `generate_scaled_corpus` Rust helper that shells out to the generator with `--count`/`--corrupt`, rewire the scale presets and stress test to use it, and add a dedicated end-to-end corpus generator test.

**Tech Stack:** Python 3 (corpus generator), Rust with `assert_cmd`/`std::process::Command` (functional tests).

---

## File Structure

| File | Changes |
|------|---------|
| `scripts/generate-test-corpus` | Add content-signature dedup to `build_random_specs` |
| `crates/voom-cli/tests/functional_tests.rs` | Add `generate_scaled_corpus` helper, rewire `populate_multi_root` to accept a corpus path, add end-to-end generator test, update stress test |

---

### Task 1: Enforce content-signature uniqueness in `build_random_specs`

**Files:**
- Modify: `scripts/generate-test-corpus` (the `build_random_specs` function, lines 290-366)

The review found that `build_random_specs` only deduplicates filenames via `used_names`, but two random files can share identical content-defining fields (duration, resolution, codec, track layout), producing the same content hash. Tests that rely on hash-based move detection break when this happens.

The content signature is: `(duration, video_codec, resolution, fps, audio_track_specs, sub_track_specs)`. Track specs must include codec and channel count for audio, format and language for subs, since those affect the generated stream.

- [ ] **Step 1: Add content-signature tracking**

In `build_random_specs`, after `used_names = set()` (line 296), add:

```python
    used_signatures = set()
```

- [ ] **Step 2: Build and check signature for each spec**

After the duration is chosen (line 354) and before `specs.append(...)` (line 356), build a content signature tuple and retry if it collides:

```python
        # Build content signature for hash uniqueness
        audio_sig = tuple(
            (t["codec"], t["channels"]) for t in audio
        )
        sub_sig = tuple(
            (s["format"], s.get("lang", "und")) for s in subs
        )
        signature = (
            duration, video["codec"], video["size"],
            video["fps"], ext, audio_sig, sub_sig,
        )

        while signature in used_signatures:
            # Bump duration until we find an unused signature
            duration = duration + 1
            signature = (
                duration, video["codec"], video["size"],
                video["fps"], ext, audio_sig, sub_sig,
            )

        used_signatures.add(signature)
```

Update the spec's `duration_override` to use the potentially bumped value:

```python
        specs.append({
            "stem": stem,
            "ext": ext,
            "video": video,
            "audio": audio,
            "subs": subs,
            "special": [],
            "duration_override": duration,
        })
```

- [ ] **Step 3: Verify uniqueness with a large count**

Run:
```bash
python3 -c "
import random, sys
sys.path.insert(0, '.')
# Load everything up to main()
src = open('scripts/generate-test-corpus').read()
exec(src.split('def main')[0])
specs = build_random_specs(100, 42, (1, 5))
sigs = set()
for s in specs:
    audio_sig = tuple((t['codec'], t['channels']) for t in s['audio'])
    sub_sig = tuple((sub['format'], sub.get('lang', 'und')) for sub in s['subs'])
    sig = (s['duration_override'], s['video']['codec'], s['video']['size'],
           s['video']['fps'], s['ext'], audio_sig, sub_sig)
    assert sig not in sigs, f'duplicate signature at {s[\"stem\"]}'
    sigs.add(sig)
print(f'{len(specs)} specs, all unique signatures')
"
```

Expected: `100 specs, all unique signatures`

- [ ] **Step 4: Commit**

```bash
git add scripts/generate-test-corpus
git commit -m "fix: enforce content-signature uniqueness in random corpus generation"
```

---

### Task 2: Add `generate_scaled_corpus` helper to functional tests

**Files:**
- Modify: `crates/voom-cli/tests/functional_tests.rs` (inside `test_lifecycle_advanced` module, helpers section)

The current `corpus_dir()` generates the shared 9-file manifest corpus once. Scale presets need a way to generate larger corpora with `--count` and `--corrupt`. Add a helper that shells out to the generator script.

- [ ] **Step 1: Add the `generate_scaled_corpus` helper**

Add this function in the helpers section of `test_lifecycle_advanced`, after the existing `set_pruning_retention` helper:

```rust
    /// Generate a scaled test corpus by invoking `scripts/generate-test-corpus`
    /// with `--count` and optionally `--corrupt`. Returns the path to the
    /// generated corpus directory.
    ///
    /// The corpus is created inside the TestEnv's tempdir so it's cleaned up
    /// automatically. Files include both the standard manifest and N random files.
    fn generate_scaled_corpus(
        env: &TestEnv,
        count: usize,
        corrupt: usize,
        seed: u64,
    ) -> PathBuf {
        let corpus_path = env._tempdir.path().join("scaled_corpus");
        std::fs::create_dir_all(&corpus_path).expect("create scaled corpus dir");

        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/generate-test-corpus");
        assert!(
            script.exists(),
            "generate-test-corpus not found at {}",
            script.display()
        );

        let mut cmd = std::process::Command::new("python3");
        cmd.arg(&script)
            .arg(&corpus_path)
            .arg("--duration")
            .arg("2")
            .arg("--seed")
            .arg(seed.to_string())
            .arg("--skip")
            .arg("av1-opus,hevc-truehd")
            .arg("--count")
            .arg(count.to_string())
            .arg("--duration-range")
            .arg("1-3");

        if corrupt > 0 {
            cmd.arg("--corrupt").arg(corrupt.to_string());
        }

        let output = cmd
            .output()
            .expect("run generate-test-corpus for scaled corpus");

        assert!(
            output.status.success(),
            "generate-test-corpus --count {count} failed (exit {:?}):\nstdout: {}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );

        // Validate that the generator actually produced the expected files
        let media_files: Vec<_> = std::fs::read_dir(&corpus_path)
            .expect("read scaled corpus dir")
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_lowercase();
                name.ends_with(".mkv") || name.ends_with(".mp4")
            })
            .collect();

        assert!(
            media_files.len() >= count,
            "generate-test-corpus produced {} media files, expected at least {count}",
            media_files.len(),
        );

        corpus_path
    }
```

- [ ] **Step 2: Add `populate_multi_root_from` helper that accepts an arbitrary corpus path**

Add after `generate_scaled_corpus`:

```rust
    /// Like `populate_multi_root`, but reads from an arbitrary corpus path
    /// instead of the shared `corpus_dir()`.
    fn populate_multi_root_from(
        env: &TestEnv,
        corpus: &Path,
        num_roots: usize,
        max_per_root: usize,
    ) -> (Vec<PathBuf>, Vec<(usize, String)>) {
        let mut roots = Vec::new();
        for i in 0..num_roots {
            let root = env._tempdir.path().join(format!("root_{i}"));
            std::fs::create_dir_all(&root).expect("create root dir");
            roots.push(root);
        }
        let mut files: Vec<_> = std::fs::read_dir(corpus)
            .expect("read corpus")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .collect();
        files.sort_by_key(|e| e.file_name());
        let mut assignments = Vec::new();
        let mut counts = vec![0usize; num_roots];
        for entry in &files {
            let target = (0..num_roots)
                .filter(|&r| max_per_root == 0 || counts[r] < max_per_root)
                .min_by_key(|&r| counts[r]);
            let Some(root_idx) = target else { break };
            let name = entry.file_name().to_string_lossy().into_owned();
            std::fs::copy(entry.path(), roots[root_idx].join(&name))
                .expect("copy file");
            assignments.push((root_idx, name));
            counts[root_idx] += 1;
        }
        (roots, assignments)
    }
```

- [ ] **Step 3: Verify it compiles**

Run:
```bash
cargo test -p voom-cli --features functional --no-run 2>&1 | tail -3
```

Expected: compiles successfully.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-cli/tests/functional_tests.rs
git commit -m "feat: add generate_scaled_corpus and populate_multi_root_from test helpers"
```

---

### Task 3: Rewire `lifecycle_iteration_stress` to use scaled corpus

**Files:**
- Modify: `crates/voom-cli/tests/functional_tests.rs` (the `lifecycle_iteration_stress` test)

The review found that `VOOM_TEST_SCALE=large` just uses "all available corpus files" (~9), giving a false sense of scale. Rewire the stress test to generate a scaled corpus via `--count` based on the preset.

- [ ] **Step 1: Update ScalePreset to include a `random_files` count**

Change the `ScalePreset` struct:

```rust
    struct ScalePreset {
        files_per_root: usize,
        num_roots: usize,
        lifecycle_iterations: usize,
        corrupt_files: usize,
        random_files: usize,
    }
```

Update the three preset arms:

```rust
            "medium" => ScalePreset {
                files_per_root: 10,
                num_roots: 3,
                lifecycle_iterations: 5,
                corrupt_files: 3,
                random_files: 20,
            },
            "large" => ScalePreset {
                files_per_root: 0,
                num_roots: 4,
                lifecycle_iterations: 10,
                corrupt_files: 5,
                random_files: 50,
            },
            _ => ScalePreset {
                files_per_root: 3,
                num_roots: 2,
                lifecycle_iterations: 3,
                corrupt_files: 1,
                random_files: 0,
            },
```

- [ ] **Step 2: Update `lifecycle_iteration_stress` to use scaled corpus when preset has random_files > 0**

Replace the beginning of the test (from `let env = TestEnv::new()` through the initial scan) with:

```rust
    #[test]
    fn lifecycle_iteration_stress() {
        require_tool!("ffprobe");
        use rand::RngExt;

        let env = TestEnv::new();
        let preset = scale_preset();

        // Use scaled corpus when the preset requests random files,
        // otherwise fall back to the shared corpus.
        let (roots, _assignments) = if preset.random_files > 0 {
            let corpus = generate_scaled_corpus(
                &env,
                preset.random_files,
                0, // no corruption for stress test
                42,
            );
            populate_multi_root_from(
                &env,
                &corpus,
                preset.num_roots,
                preset.files_per_root,
            )
        } else {
            populate_multi_root(&env, preset.num_roots, preset.files_per_root)
        };

        // Initial scan of all roots
        for root in &roots {
```

The rest of the test remains unchanged.

- [ ] **Step 3: Run with default preset (no random files)**

Run:
```bash
cargo test -p voom-cli --features functional -- test_lifecycle_advanced::lifecycle_iteration_stress --test-threads=1 2>&1 | tail -5
```

Expected: passes (uses shared corpus as before).

- [ ] **Step 4: Run with medium preset (20 random files)**

Run:
```bash
VOOM_TEST_SCALE=medium cargo test -p voom-cli --features functional -- test_lifecycle_advanced::lifecycle_iteration_stress --test-threads=1 2>&1 | tail -5
```

Expected: passes with more files and more iterations. Takes longer (~1-2 minutes).

- [ ] **Step 5: Commit**

```bash
git add crates/voom-cli/tests/functional_tests.rs
git commit -m "feat: lifecycle stress test uses scaled corpus for medium/large presets"
```

---

### Task 4: Add end-to-end corpus generator test with `--count` and `--corrupt`

**Files:**
- Modify: `crates/voom-cli/tests/functional_tests.rs` (new test in `test_lifecycle_advanced`)

The review found that corruption tests manually create bogus files instead of exercising `generate-test-corpus --corrupt`. The selection logic, percentage parsing, seeding, and all 5 corruption types are untested end-to-end. Add a test that generates a corpus with `--count` and `--corrupt`, then scans it and processes it.

- [ ] **Step 1: Add the end-to-end test**

Add at the end of the `test_lifecycle_advanced` module, after the G section:

```rust
    // ───────────────────────────────────────────────────────────────────
    // H. End-to-end corpus generator integration
    // ───────────────────────────────────────────────────────────────────

    #[test]
    fn end_to_end_scaled_corpus_with_corruption() {
        require_tool!("ffprobe");
        let env = TestEnv::new();
        let preset = scale_preset();

        // Generate a corpus with random files and some corrupted
        let count = std::cmp::max(preset.corrupt_files + 2, 5);
        let corrupt = preset.corrupt_files;
        let corpus = generate_scaled_corpus(&env, count, corrupt, 42);

        // Verify the generator produced files
        let generated: Vec<_> = std::fs::read_dir(&corpus)
            .expect("read corpus dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .collect();

        // Should have manifest files + random files (some may have failed)
        assert!(
            generated.len() >= count,
            "expected at least {count} random files, got {}",
            generated.len()
        );

        // Copy all files to the test media dir
        let media = env.media_dir();
        std::fs::create_dir_all(&media).unwrap();
        for entry in &generated {
            let name = entry.file_name();
            std::fs::copy(entry.path(), media.join(&name)).unwrap();
        }

        // Scan — should complete without crashing, track valid files
        env.voom()
            .args(["scan", media.to_str().unwrap()])
            .timeout(scan_timeout())
            .assert()
            .success();

        let db = env.db_path();
        let active = count_by_status(&db, "active");
        assert!(
            active >= 1,
            "at least some valid files should be tracked after scan"
        );

        // Corrupt files should NOT prevent valid files from being tracked.
        // With N corrupt files, at least (total - N) should be active.
        let min_expected = (generated.len() - corrupt) as i64;
        assert!(
            active >= min_expected - 2, // allow for edge cases
            "expected ~{min_expected} active files, got {active} \
             ({corrupt} corrupt out of {} total)",
            generated.len()
        );

        // Verify discovery transitions were recorded for the valid files
        let discovery_count = count_transitions_by_source(&db, "discovery");
        assert!(
            discovery_count >= active,
            "each active file should have at least one discovery transition, \
             got {discovery_count} for {active} active files"
        );

        // Process with --plan-only to verify:
        // - Valid files produce plans (corrupt files don't abort the batch)
        // - Plans are valid JSON with at least one entry
        let policy = env.write_policy("test", TEST_POLICY);

        let process_output = env
            .voom()
            .args([
                "process",
                media.to_str().unwrap(),
                "--policy",
                policy.to_str().unwrap(),
                "--plan-only",
            ])
            .timeout(process_timeout())
            .output()
            .expect("run process --plan-only");

        assert!(
            process_output.status.success(),
            "process --plan-only failed: {}",
            String::from_utf8_lossy(&process_output.stderr),
        );

        // --plan-only outputs a JSON array of plans to stdout.
        // At least one valid file should produce a plan.
        let stdout = String::from_utf8_lossy(&process_output.stdout);
        let plans: serde_json::Value =
            serde_json::from_str(&stdout).expect("plan-only output should be valid JSON");
        let plan_count = plans.as_array().map_or(0, |a| a.len());
        assert!(
            plan_count >= 1,
            "process --plan-only should produce plans for valid files, \
             got {plan_count} plans (with {active} active files scanned)"
        );
    }
```

- [ ] **Step 2: Run the test**

Run:
```bash
cargo test -p voom-cli --features functional -- test_lifecycle_advanced::end_to_end_scaled_corpus --test-threads=1 2>&1 | tail -10
```

Expected: passes. Shows the corpus being generated, scanned, with valid files tracked.

- [ ] **Step 3: Run with medium preset for more coverage**

Run:
```bash
VOOM_TEST_SCALE=medium cargo test -p voom-cli --features functional -- test_lifecycle_advanced::end_to_end_scaled_corpus --test-threads=1 2>&1 | tail -10
```

Expected: passes with more files and more corruption targets.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-cli/tests/functional_tests.rs
git commit -m "test: add end-to-end corpus generator test with --count and --corrupt"
```

---

### Task 5: Update design spec to remove false uniqueness claim

**Files:**
- Modify: `docs/specs/2026-04-03-lifecycle-functional-tests-design.md`

- [ ] **Step 1: Update the uniqueness section**

Find the line:

```
**Uniqueness guarantee** — each file gets a unique name (collision-checked) and unique combination of duration + resolution + track layout, ensuring distinct content hashes even at the same duration.
```

Replace with:

```
**Uniqueness guarantee** — each file gets a unique name (collision-checked) and a unique content signature (duration + codec + resolution + fps + container + track layout). If a signature collision occurs, the duration is bumped to force a distinct content hash.
```

- [ ] **Step 2: Commit**

```bash
git add docs/specs/2026-04-03-lifecycle-functional-tests-design.md
git commit -m "docs: update spec to reflect enforced content-signature uniqueness"
```
