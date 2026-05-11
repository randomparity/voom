use std::path::Path;

use anyhow::Context;
use anyhow::Result;

use crate::commands::bug_report::collect::BugReportBundle;

pub fn write_bundle(bundle: &BugReportBundle) -> Result<()> {
    prepare_output_dir(&bundle.out_dir)?;
    write_report_markdown(bundle)?;
    write_report_json(bundle)?;
    write_json_file(
        &bundle.out_dir.join("redactions.public.json"),
        &bundle.redactions,
    )?;
    write_json_file(
        &bundle.out_dir.join("redactions.local.json"),
        &serde_json::json!({
            "warning": "Private redaction map. Do not upload this file.",
            "mappings": bundle.private_redactions,
        }),
    )?;
    write_metadata(&bundle.out_dir)?;
    std::fs::write(
        bundle.out_dir.join("README.txt"),
        "Review report.md and report.json before sharing. \
         redactions.local.json contains private original values and is never uploaded by VOOM.\n",
    )
    .context("failed to write bug report README")?;
    println!(
        "Bug report generated at {}. Review report.md and report.json before upload.",
        bundle.out_dir.display()
    );
    Ok(())
}

fn prepare_output_dir(out_dir: &Path) -> Result<()> {
    if !out_dir.exists() {
        std::fs::create_dir_all(out_dir).context("failed to create bug report directory")?;
        return Ok(());
    }

    if !out_dir.is_dir() {
        anyhow::bail!(
            "bug report output path is not a directory: {}",
            out_dir.display()
        );
    }

    let mut entries = std::fs::read_dir(out_dir)
        .context("failed to read bug report output directory")?
        .peekable();
    if entries.peek().is_none() {
        return Ok(());
    }

    let metadata_path = out_dir.join("metadata.json");
    let metadata = std::fs::read_to_string(&metadata_path).with_context(|| {
        format!(
            "refusing to write into non-empty directory without {}",
            metadata_path.display()
        )
    })?;
    let value: serde_json::Value =
        serde_json::from_str(&metadata).context("failed to parse existing metadata.json")?;
    if value["kind"] != "voom_bug_report" {
        anyhow::bail!(
            "refusing to overwrite directory not marked as a VOOM bug report: {}",
            out_dir.display()
        );
    }

    for file in [
        "report.md",
        "report.json",
        "redactions.public.json",
        "redactions.local.json",
        "README.txt",
        "metadata.json",
    ] {
        let path = out_dir.join(file);
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("failed to remove previous {}", path.display()))?;
        }
    }

    Ok(())
}

fn write_report_markdown(bundle: &BugReportBundle) -> Result<()> {
    let mut report = String::new();
    report.push_str("# VOOM Bug Report\n\n");
    report.push_str("## Summary\n\n");
    report.push_str(&format!(
        "- Generated at: {}\n",
        bundle.summary.generated_at
    ));
    if let Some(session) = &bundle.summary.session {
        report.push_str(&format!("- Session: {session}\n"));
    }
    if let Some(library) = &bundle.summary.library {
        report.push_str(&format!("- Library: {library}\n"));
    }
    report.push_str("\n## Environment\n\n");
    report.push_str(&format!(
        "- VOOM: {}\n- OS: {}\n- Arch: {}\n- Current dir: {}\n",
        bundle.environment.product_version,
        bundle.environment.os,
        bundle.environment.arch,
        bundle.environment.current_dir
    ));
    if let Some(policy) = &bundle.policy {
        report.push_str("\n## Policy\n\n");
        report.push_str(&format!("Path: `{}`\n\n", policy.path));
        report.push_str("```voom\n");
        report.push_str(&policy.contents);
        report.push_str("\n```\n");
    }
    report.push_str("\n## Storage\n\n");
    report.push_str("```json\n");
    report.push_str(&serde_json::to_string_pretty(&bundle.storage)?);
    report.push_str("\n```\n");

    std::fs::write(bundle.out_dir.join("report.md"), report)
        .context("failed to write bug report markdown")
}

fn write_report_json(bundle: &BugReportBundle) -> Result<()> {
    let public_bundle = serde_json::json!({
        "summary": bundle.summary,
        "environment": bundle.environment,
        "config": bundle.config,
        "policy": bundle.policy,
        "storage": bundle.storage,
        "redactions": bundle.redactions,
    });
    write_json_file(&bundle.out_dir.join("report.json"), &public_bundle)
}

fn write_metadata(out_dir: &Path) -> Result<()> {
    write_json_file(
        &out_dir.join("metadata.json"),
        &serde_json::json!({
            "kind": "voom_bug_report",
            "version": 1,
        }),
    )
}

fn write_json_file(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let json = serde_json::to_string_pretty(value)?;
    std::fs::write(path, format!("{json}\n"))
        .with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::bug_report::collect::{
        BugReportSummary, EnvironmentCapture, PolicyCapture, StorageCapture,
    };
    use crate::commands::bug_report::redactor::Redactor;
    use std::collections::BTreeMap;

    #[test]
    fn write_bundle_creates_sanitized_files_and_private_map() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = test_bundle(dir.path());

        write_bundle(&bundle).unwrap();

        let report = std::fs::read_to_string(dir.path().join("report.md")).unwrap();
        let data = std::fs::read_to_string(dir.path().join("report.json")).unwrap();
        let private = std::fs::read_to_string(dir.path().join("redactions.local.json")).unwrap();

        assert!(report.contains("video000.mkv"));
        assert!(data.contains("video000.mkv"));
        assert!(private.contains("The Movie (2026).mkv"));
        assert!(!report.contains("The Movie (2026).mkv"));
        assert!(!data.contains("The Movie (2026).mkv"));
    }

    fn test_bundle(out_dir: &std::path::Path) -> BugReportBundle {
        let mut redactor = Redactor::default();
        let policy_contents = redactor.redact_text("process The Movie (2026).mkv");
        let redactions = redactor.report();
        let private_redactions = redactor.private_mappings();

        BugReportBundle {
            out_dir: out_dir.to_path_buf(),
            summary: BugReportSummary {
                generated_at: "2026-05-11T00:00:00Z".to_string(),
                session: None,
                library: None,
            },
            environment: EnvironmentCapture {
                product_version: "test".to_string(),
                os: "test-os".to_string(),
                arch: "test-arch".to_string(),
                current_dir: "/tmp".to_string(),
                env: BTreeMap::new(),
            },
            config: serde_json::json!({"data_dir": "/tmp"}),
            policy: Some(PolicyCapture {
                path: "policy.voom".to_string(),
                contents: policy_contents,
            }),
            storage: StorageCapture::Unavailable {
                error: "not opened".to_string(),
            },
            redactions,
            private_redactions,
        }
    }
}
