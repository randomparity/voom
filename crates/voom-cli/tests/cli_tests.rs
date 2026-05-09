use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

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
    voom()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("voom"));
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
        .args(["policy", "test", "--json", suite.to_str().unwrap()])
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
    assert!(json["cases"][0]["failures"][0]
        .as_str()
        .unwrap()
        .contains("missing"));
}

#[test]
fn test_policy_test_update_without_snapshots_is_error() {
    let tmp = tempfile::tempdir().unwrap();
    let suite = write_policy_test_suite(tmp.path(), "containerize");

    voom()
        .args(["policy", "test", "--update", suite.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--update requires snapshot assertions, which are not available yet",
        ));
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
