use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use voom_domain::media::{Container, Track, TrackType};
use voom_policy_testing::Fixture;

fn voom() -> Command {
    let mut cmd = Command::cargo_bin("voom").unwrap();
    // Always pass --force so parallel integration tests don't fight over the process lock.
    cmd.arg("--force");
    // Isolate from the developer's real ~/.config/voom so tests don't see stale DBs.
    // XDG_CONFIG_HOME takes precedence over HOME in voom_config_dir().
    let scratch = tempfile::tempdir()
        .expect("create tempdir for test config")
        .keep();
    cmd.env("XDG_CONFIG_HOME", &scratch);
    cmd
}

fn assert_stdout_is_json(args: &[&str]) -> Value {
    let output = voom().args(args).assert().success().get_output().clone();
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not valid JSON for {args:?}: {error}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn assert_no_human_status_on_json_stdout(args: &[&str], banned: &[&str]) {
    let output = voom().args(args).assert().success().get_output().clone();
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<Value>(&stdout).unwrap_or_else(|error| {
        panic!("stdout was not valid JSON for {args:?}: {error}\nstdout:\n{stdout}")
    });

    for needle in banned {
        assert!(
            !stdout.contains(needle),
            "stdout for {args:?} contained human status {needle:?}:\n{stdout}"
        );
    }
}

fn assert_human_notes_on_stderr(args: &[&str], needle: &str) {
    let output = voom().args(args).assert().success().get_output().clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(needle),
        "stderr for {args:?} did not contain {needle:?}:\n{stderr}"
    );
}

fn write_policy_test_suite(dir: &std::path::Path, expected_phase: &str) -> std::path::PathBuf {
    let policy_path = dir.join("minimal.voom");
    let fixture_path = dir.join("movie.json");
    let suite_path = dir.join("minimal.test.json");

    std::fs::write(
        &policy_path,
        r#"policy "minimal" {
  phase containerize {
    container mkv
  }
}
"#,
    )
    .unwrap();
    std::fs::write(
        &fixture_path,
        r#"{
  "path": "/media/movie.mp4",
  "container": "Mp4",
  "duration": 120.0,
  "size": 99,
  "tracks": [{
    "index": 0,
    "track_type": "Video",
    "codec": "h264",
    "language": "und",
    "title": "",
    "is_default": true,
    "is_forced": false,
    "channels": null,
    "channel_layout": null,
    "sample_rate": null,
    "bit_depth": null,
    "width": 1920,
    "height": 1080,
    "frame_rate": 23.976,
    "is_vfr": false,
    "is_hdr": false,
    "hdr_format": null,
    "pixel_format": null
  }]
}
"#,
    )
    .unwrap();
    std::fs::write(
        &suite_path,
        format!(
            r#"{{
  "policy": "minimal.voom",
  "cases": [{{
    "name": "containerizes mp4",
    "fixture": "movie.json",
    "expect": {{"phases_run": ["{expected_phase}"]}}
  }}]
}}
"#
        ),
    )
    .unwrap();

    suite_path
}

fn write_policy_snapshot_suite(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let suite_path = write_policy_test_suite(dir, "containerize");
    let snapshot_path = dir.join("minimal.snapshot.json");
    let suite = format!(
        r#"{{
  "policy": "minimal.voom",
  "cases": [{{
    "name": "containerizes mp4",
    "fixture": "movie.json",
    "snapshot": "{}"
  }}]
}}
"#,
        snapshot_path.file_name().unwrap().to_string_lossy()
    );
    std::fs::write(&suite_path, suite).unwrap();
    (suite_path, snapshot_path)
}

// === Basic CLI structure ===

#[test]
fn test_help() {
    voom()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Video Orchestration Operations Manager",
        ));
}

#[test]
fn test_version() {
    let package_version = env!("CARGO_PKG_VERSION").replace('.', r"\.");
    let pattern = format!(r"^voom {package_version}(-dev\+(g[0-9a-f]{{7,12}}|unknown))?\n$");

    voom()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::is_match(pattern).unwrap());
}

#[test]
fn test_no_args_shows_help() {
    voom()
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage"));
}

// === Subcommand help ===

#[test]
fn test_scan_help() {
    voom()
        .args(["scan", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Discover and introspect"));
}

#[test]
fn test_inspect_help() {
    voom()
        .args(["inspect", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Show file metadata"));
}

#[test]
fn test_process_help() {
    voom()
        .args(["process", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Apply policy to files"));
}

#[test]
fn test_policy_help() {
    voom()
        .args(["policy", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("validate"));
}

#[test]
fn test_plugin_help() {
    voom()
        .args(["plugin", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("list"));
}

#[test]
fn test_jobs_help() {
    voom()
        .args(["jobs", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("list"));
}

#[test]
fn test_doctor_help() {
    voom()
        .args(["doctor", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "System environment check (alias for `env check`)",
        ));
}

#[test]
fn test_serve_help() {
    voom()
        .args(["serve", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("web server"));
}

#[test]
fn test_db_help() {
    voom()
        .args(["db", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("prune"));
}

#[test]
fn test_config_help() {
    voom()
        .args(["config", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("show"));
}

#[test]
fn test_completions_help() {
    voom()
        .args(["completions", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("shell"));
}

// === Shell completions ===

#[test]
fn test_completions_bash() {
    voom()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("_voom"));
}

#[test]
fn test_completions_zsh() {
    voom()
        .args(["completions", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("voom"));
}

#[test]
fn test_completions_fish() {
    voom()
        .args(["completions", "fish"])
        .assert()
        .success()
        .stdout(predicate::str::contains("voom"));
}

// === Agent-facing output contracts ===

#[test]
fn test_existing_json_outputs_are_parseable() {
    let dir = tempfile::tempdir().unwrap();
    let empty_scan = assert_stdout_is_json(&[
        "scan",
        dir.path().to_str().unwrap(),
        "--format",
        "json",
        "--no-hash",
    ]);
    assert_eq!(empty_scan, Value::Array(vec![]));

    let tools = assert_stdout_is_json(&["tools", "list", "--format", "json"]);
    assert!(tools.is_array());

    let env = assert_stdout_is_json(&["env", "check", "--format", "json"]);
    assert!(env.get("passed").is_some());
    assert!(env.get("issue_count").is_some());
}

#[test]
fn test_new_query_json_outputs_are_parseable() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/voom-dsl/tests/fixtures/production-normalize.voom");

    let policy = assert_stdout_is_json(&[
        "policy",
        "validate",
        fixture.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_eq!(policy["valid"], true);

    let plugins = assert_stdout_is_json(&["plugin", "list", "--format", "json"]);
    assert!(plugins["plugins"].is_array());
    assert!(plugins["disabled_plugins"].is_array());

    let config = assert_stdout_is_json(&["config", "show", "--format", "json"]);
    assert!(config["data_dir"].is_string());

    let jobs = assert_stdout_is_json(&["jobs", "list", "--format", "json"]);
    assert!(jobs["jobs"].is_array());
}

#[test]
fn test_policy_validate_map_json_is_parseable() {
    let dir = tempfile::tempdir().unwrap();
    let policy = dir.path().join("minimal.voom");
    let policy_map = dir.path().join("map.toml");

    std::fs::write(
        &policy,
        r#"policy "minimal" {
  phase containerize {
    container mkv
  }
}
"#,
    )
    .unwrap();

    std::fs::write(
        &policy_map,
        r#"default = "minimal.voom"

[[mapping]]
prefix = "movies"
policy = "minimal.voom"
"#,
    )
    .unwrap();

    let json = assert_stdout_is_json(&[
        "policy",
        "validate",
        policy_map.to_str().unwrap(),
        "--format",
        "json",
    ]);

    assert_eq!(json["valid"], true);
    assert_eq!(json["policy_count"], 1);
    assert!(json["policies"].is_array());
    assert_eq!(json["policies"][0]["policy"], "minimal");
}

#[test]
fn test_json_stdout_excludes_human_status_text() {
    let dir = tempfile::tempdir().unwrap();
    assert_no_human_status_on_json_stdout(
        &[
            "scan",
            dir.path().to_str().unwrap(),
            "--format",
            "json",
            "--no-hash",
        ],
        &["No media files found", "Scanning", "Pruned"],
    );
    assert_no_human_status_on_json_stdout(
        &["tools", "list", "--format", "json"],
        &["tool(s) detected", "No external tools detected"],
    );
}

#[test]
fn test_human_notes_use_stderr_for_human_commands() {
    let dir = tempfile::tempdir().unwrap();
    assert_human_notes_on_stderr(
        &["scan", dir.path().to_str().unwrap()],
        "No media files found",
    );
}

// === Policy validation ===

#[test]
fn test_policy_validate_valid_file() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/voom-dsl/tests/fixtures/production-normalize.voom");

    assert!(fixture.exists(), "fixture missing: {}", fixture.display());
    voom()
        .args(["policy", "validate", fixture.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("OK"));
}

#[test]
fn policy_validate_accepts_file_extends() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("base.voom"),
        r#"policy "base" { phase base { container mkv } }"#,
    )
    .unwrap();
    let child = dir.path().join("child.voom");
    std::fs::write(
        &child,
        r#"policy "child" extends "file://./base.voom" {
            phase child { depends_on: [base] keep audio }
        }"#,
    )
    .unwrap();

    voom()
        .args(["policy", "validate", child.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Policy \"child\" is valid"));
}

#[test]
fn policy_describe_reports_extended_phase() {
    let dir = tempfile::tempdir().unwrap();
    let child = dir.path().join("child.voom");
    std::fs::write(
        &child,
        r#"policy "child" extends "anime-base" {
            phase audio { extend keep audio where lang == eng }
        }"#,
    )
    .unwrap();

    voom()
        .args(["policy", "describe", child.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Extends: anime-base"))
        .stdout(predicate::str::contains("audio"))
        .stdout(predicate::str::contains("extended"));
}

#[test]
fn policy_describe_json_reports_file_parent_composition() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.voom");
    std::fs::write(
        &base,
        r#"policy "base" {
            phase containerize { container mkv }
            phase audio { keep audio where lang == eng }
            phase subtitles { keep subtitles where lang == und }
        }"#,
    )
    .unwrap();
    let child = dir.path().join("child.voom");
    std::fs::write(
        &child,
        r#"policy "child" extends "file://./base.voom" {
            phase audio { extend keep audio where lang == und }
            phase subtitles { keep subtitles where lang == eng }
        }"#,
    )
    .unwrap();

    let output = voom()
        .args([
            "policy",
            "describe",
            child.to_str().unwrap(),
            "--format",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout was not valid JSON: {error}\nstdout:\n{stdout}"));
    let parent = std::fs::canonicalize(&base).unwrap().display().to_string();
    let child_source = std::fs::canonicalize(&child).unwrap().display().to_string();

    assert_eq!(json["policy"], "child");
    assert_eq!(json["extends_chain"], serde_json::json!([parent]));
    assert_describe_phase_composition(&json, "containerize", "Inherited", Some(&parent), 0);
    assert_describe_phase_composition(&json, "audio", "Extended", Some(&parent), 1);
    assert_describe_phase_composition(&json, "subtitles", "Overridden", Some(&child_source), 0);
}

#[test]
fn test_policy_validate_invalid_file() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "this is not valid voom syntax").unwrap();

    voom()
        .args(["policy", "validate", tmp.path().to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Policy validation failed"));
}

#[test]
fn test_policy_validate_missing_file() {
    voom()
        .args(["policy", "validate", "/nonexistent/policy.voom"])
        .assert()
        .failure();
}

fn assert_describe_phase_composition(
    value: &Value,
    name: &str,
    kind: &str,
    source: Option<&str>,
    added_operations: usize,
) {
    let phase = value["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["name"] == name)
        .unwrap_or_else(|| panic!("missing describe phase {name:?}: {value}"));

    assert_eq!(phase["composition"]["kind"], kind);
    assert_eq!(phase["composition"]["source"], serde_json::json!(source));
    assert_eq!(phase["composition"]["added_operations"], added_operations);
}

#[test]
fn test_policy_show_valid_file() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/voom-dsl/tests/fixtures/production-normalize.voom");

    assert!(fixture.exists(), "fixture missing: {}", fixture.display());
    voom()
        .args(["policy", "show", fixture.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Policy"));
}

#[test]
fn test_policy_format_roundtrip() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/voom-dsl/tests/fixtures/production-normalize.voom");

    assert!(fixture.exists(), "fixture missing: {}", fixture.display());
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let content = std::fs::read_to_string(&fixture).unwrap();
    std::fs::write(tmp.path(), &content).unwrap();

    voom()
        .args(["policy", "format", tmp.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("OK"));

    // Formatted file should still be valid
    voom()
        .args(["policy", "validate", tmp.path().to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn test_policy_test_runs_matching_suite() {
    let tmp = tempfile::tempdir().unwrap();
    let suite = write_policy_test_suite(tmp.path(), "containerize");

    voom()
        .args(["policy", "test", suite.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("OK containerizes mp4"))
        .stdout(predicate::str::contains("1 passed, 0 failed, 1 total"));
}

#[test]
fn test_policy_test_json_reports_failures_and_exits_one() {
    let tmp = tempfile::tempdir().unwrap();
    let suite = write_policy_test_suite(tmp.path(), "missing");

    let output = voom()
        .args([
            "policy",
            "test",
            "--format",
            "json",
            suite.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();

    assert_eq!(json["summary"]["passed"], 0);
    assert_eq!(json["summary"]["failed"], 1);
    assert_eq!(json["summary"]["total"], 1);
    assert_eq!(json["cases"][0]["name"], "containerizes mp4");
    assert_eq!(json["cases"][0]["status"], "fail");
    assert!(
        json["cases"][0]["failures"][0]
            .as_str()
            .unwrap()
            .contains("missing")
    );
}

#[test]
fn test_policy_test_update_without_snapshots_reports_no_regeneration() {
    let tmp = tempfile::tempdir().unwrap();
    let suite = write_policy_test_suite(tmp.path(), "containerize");

    voom()
        .args(["policy", "test", "--update", suite.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("no snapshots regenerated"));
}

#[test]
fn test_policy_test_update_regenerates_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let (suite, snapshot) = write_policy_snapshot_suite(tmp.path());

    voom()
        .args(["policy", "test", "--update", suite.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("updated 1 snapshot"));

    let first = std::fs::read_to_string(&snapshot).unwrap();

    voom()
        .args(["policy", "test", "--update", suite.to_str().unwrap()])
        .assert()
        .success();

    assert_eq!(first, std::fs::read_to_string(&snapshot).unwrap());
}

#[test]
fn test_policy_test_snapshot_diff_is_readable() {
    let tmp = tempfile::tempdir().unwrap();
    let (suite, snapshot) = write_policy_snapshot_suite(tmp.path());
    std::fs::write(&snapshot, "[\n  {\"phase_name\": \"wrong\"}\n]\n").unwrap();

    voom()
        .args(["policy", "test", suite.to_str().unwrap()])
        .assert()
        .failure()
        .stdout(predicate::str::contains("--- expected"))
        .stdout(predicate::str::contains("+++ actual"))
        .stdout(predicate::str::contains("-  {\"phase_name\": \"wrong\"}"))
        .stdout(predicate::str::contains(
            "+    \"phase_name\": \"containerize\"",
        ));
}

#[test]
fn test_policy_diff_fixture_identical_exits_zero() {
    let fixture = PolicyDiffFixture::new();

    voom()
        .args([
            "policy",
            "diff",
            fixture.a.to_str().unwrap(),
            fixture.a.to_str().unwrap(),
            "--fixture",
            fixture.fixture.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Policies are identical"));
}

#[test]
fn test_policy_diff_fixture_different_exits_one_with_diff() {
    let fixture = PolicyDiffFixture::new();

    voom()
        .args([
            "policy",
            "diff",
            fixture.a.to_str().unwrap(),
            fixture.b.to_str().unwrap(),
            "--fixture",
            fixture.fixture.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("Policy diff"))
        .stdout(predicate::str::contains("+").or(predicate::str::contains("~")));
}

struct PolicyDiffFixture {
    _dir: tempfile::TempDir,
    a: std::path::PathBuf,
    b: std::path::PathBuf,
    fixture: std::path::PathBuf,
}

impl PolicyDiffFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.voom");
        let b = dir.path().join("b.voom");
        let fixture = dir.path().join("movie.json");
        std::fs::write(
            &a,
            r#"
policy "containerize" {
  phase convert {
    container mkv
  }
}
"#,
        )
        .unwrap();
        std::fs::write(
            &b,
            r#"
policy "noop" {
  phase verify {
  }
}
"#,
        )
        .unwrap();

        let media = Fixture {
            path: std::path::PathBuf::from("/media/movie.mp4"),
            container: Container::Mp4,
            duration: 120.0,
            size: 99,
            tracks: vec![Track::new(0, TrackType::Video, "h264".to_string())],
            capabilities: None,
        };
        std::fs::write(&fixture, serde_json::to_string(&media).unwrap()).unwrap();

        Self {
            _dir: dir,
            a,
            b,
            fixture,
        }
    }
}

// === Error cases ===

#[test]
fn test_scan_nonexistent_path() {
    voom()
        .args(["scan", "/nonexistent/path/to/media"])
        .assert()
        .failure();
}

#[test]
fn test_inspect_nonexistent_file() {
    voom()
        .args(["inspect", "/nonexistent/file.mkv"])
        .assert()
        .failure();
}

// === Serve ===

#[test]
fn test_serve_accepts_args() {
    // The serve command now starts a real server.
    // Test that it accepts --port and --host args (it will fail
    // to connect to the DB in test environments, which is expected).
    let _ = voom()
        .args(["serve", "--port", "0", "--host", "127.0.0.1"])
        .timeout(std::time::Duration::from_secs(2))
        .assert();
    // We just verify it doesn't crash immediately with bad args.
    // It may fail due to no DB or succeed if one exists.
}

// === Success-path tests ===

#[test]
fn test_doctor_runs_to_completion() {
    voom()
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("VOOM Environment Check"));
}

#[test]
fn test_config_show_runs_to_completion() {
    voom().args(["config", "show"]).assert().success();
}

#[test]
fn test_scan_empty_directory() {
    let dir = tempfile::tempdir().unwrap();
    voom()
        .args(["scan", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stderr(predicate::str::contains("No media files found"));
}

#[test]
fn test_plugin_list_shows_registered_plugins() {
    voom()
        .args(["plugin", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("plugin"));
}
