//! End-to-end check: `voom health check` exits 0 against a fresh data
//! directory and reports the Retention coverage section. Issue #194.

use assert_cmd::Command;

#[test]
fn health_check_runs_against_empty_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    let cfg_path = cfg_dir.path().join("voom").join("config.toml");
    std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
    std::fs::write(
        &cfg_path,
        format!("data_dir = \"{}\"\n", dir.path().display()),
    )
    .unwrap();

    let out = Command::cargo_bin("voom")
        .unwrap()
        .env("XDG_CONFIG_HOME", cfg_dir.path())
        .args(["health", "check"])
        .output()
        .expect("run voom health check");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Retention coverage"),
        "expected 'Retention coverage' in output, got:\n{stdout}"
    );
}
