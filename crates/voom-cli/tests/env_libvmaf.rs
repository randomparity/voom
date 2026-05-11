//! Optional live-ffmpeg coverage for `voom env check` libvmaf reporting.

use assert_cmd::Command;

fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|output| output.status.success())
}

#[test]
fn env_check_json_reports_libvmaf_when_ffmpeg_is_present() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not found on PATH");
        return;
    }

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
        .args(["env", "check", "--format", "json"])
        .output()
        .expect("run voom env check --format json");

    assert!(out.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("env check JSON output");
    assert!(value.get("vmaf_supported").is_some());
    assert!(value.get("vmaf_model_status").is_some());
}
