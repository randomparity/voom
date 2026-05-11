use std::{fs, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=GITHUB_REF_NAME");
    println!("cargo:rerun-if-env-changed=GITHUB_REF_TYPE");
    emit_git_rerun_instructions();

    let package_version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION is set");
    let product_version = product_version(&package_version);
    println!("cargo:rustc-env=VOOM_PRODUCT_VERSION={product_version}");
}

fn emit_git_rerun_instructions() {
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/packed-refs");
    println!("cargo:rerun-if-changed=../../.git/refs/tags");

    let Ok(head) = fs::read_to_string("../../.git/HEAD") else {
        return;
    };
    let Some(reference) = head.trim().strip_prefix("ref: ") else {
        return;
    };

    println!("cargo:rerun-if-changed=../../.git/{reference}");
}

fn product_version(package_version: &str) -> String {
    let expected_tag = format!("v{package_version}");

    if github_ref_is_release_tag(&expected_tag) || git_head_is_release_tag(&expected_tag) {
        return package_version.to_owned();
    }

    match git_short_sha() {
        Some(sha) => format!("{package_version}-dev+g{sha}"),
        None => format!("{package_version}-dev+unknown"),
    }
}

fn github_ref_is_release_tag(expected_tag: &str) -> bool {
    std::env::var("GITHUB_REF_TYPE").as_deref() == Ok("tag")
        && std::env::var("GITHUB_REF_NAME").as_deref() == Ok(expected_tag)
}

fn git_head_is_release_tag(expected_tag: &str) -> bool {
    git_output(&[
        "describe",
        "--tags",
        "--exact-match",
        "--match",
        "v[0-9]*.[0-9]*.[0-9]*",
    ])
    .is_some_and(|tag| tag == expected_tag)
}

fn git_short_sha() -> Option<String> {
    git_output(&["rev-parse", "--short=12", "HEAD"])
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    let trimmed = stdout.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}
