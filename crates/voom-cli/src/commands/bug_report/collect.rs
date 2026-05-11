use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;
use voom_domain::storage::{EventLogFilters, JobFilters};

use crate::cli::BugReportGenerateArgs;
use crate::commands::bug_report::redactor::{PrivateRedactionMapping, RedactionReport, Redactor};

#[derive(Debug, Serialize)]
pub struct BugReportBundle {
    pub out_dir: PathBuf,
    pub summary: BugReportSummary,
    pub environment: EnvironmentCapture,
    pub config: serde_json::Value,
    pub policy: Option<PolicyCapture>,
    pub storage: StorageCapture,
    pub redactions: RedactionReport,
    pub private_redactions: Vec<PrivateRedactionMapping>,
}

#[derive(Debug, Serialize)]
pub struct BugReportSummary {
    pub generated_at: String,
    pub session: Option<String>,
    pub library: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EnvironmentCapture {
    pub product_version: String,
    pub os: String,
    pub arch: String,
    pub current_dir: String,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct PolicyCapture {
    pub path: String,
    pub contents: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StorageCapture {
    Available {
        table_row_counts: Vec<(String, u64)>,
        jobs: Vec<serde_json::Value>,
        events: Vec<serde_json::Value>,
        health_checks: Vec<serde_json::Value>,
    },
    Unavailable {
        error: String,
    },
}

pub fn collect(args: BugReportGenerateArgs) -> Result<BugReportBundle> {
    let mut redactor = Redactor::default();
    let environment = collect_environment(&mut redactor);
    let config = crate::config::load_config()?;
    let config_value = redactor.redact_json(serde_json::to_value(&config)?);
    let policy = args
        .policy
        .as_deref()
        .map(|path| collect_policy(path, &mut redactor))
        .transpose()?;
    let library = args
        .library
        .as_ref()
        .map(|path| redactor.redact_text(&path.display().to_string()));
    let storage = collect_storage(&config, &args, &mut redactor);

    Ok(BugReportBundle {
        out_dir: args.out,
        summary: BugReportSummary {
            generated_at: chrono::Utc::now().to_rfc3339(),
            session: args.session,
            library,
        },
        environment,
        config: config_value,
        policy,
        storage,
        redactions: redactor.report(),
        private_redactions: redactor.private_mappings(),
    })
}

pub fn collect_environment(redactor: &mut Redactor) -> EnvironmentCapture {
    let env = std::env::vars()
        .filter(|(key, _)| include_env_key(key))
        .map(|(key, value)| (key, redactor.redact_text(&value)))
        .collect();
    let current_dir = std::env::current_dir()
        .map(|path| redactor.redact_text(&path.display().to_string()))
        .unwrap_or_else(|e| redactor.redact_text(&format!("unavailable: {e}")));

    EnvironmentCapture {
        product_version: env!("VOOM_PRODUCT_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        current_dir,
        env,
    }
}

pub fn collect_policy(path: &Path, redactor: &mut Redactor) -> Result<PolicyCapture> {
    let contents = std::fs::read_to_string(path)?;
    Ok(PolicyCapture {
        path: redactor.redact_text(&path.display().to_string()),
        contents: redactor.redact_text(&contents),
    })
}

fn collect_storage(
    config: &crate::config::AppConfig,
    args: &BugReportGenerateArgs,
    redactor: &mut Redactor,
) -> StorageCapture {
    let store = match crate::app::open_store(config) {
        Ok(store) => store,
        Err(e) => {
            return StorageCapture::Unavailable {
                error: redactor.redact_text(&e.to_string()),
            };
        }
    };

    let table_row_counts = store.table_row_counts().unwrap_or_default();

    let mut job_filters = JobFilters::default();
    job_filters.limit = Some(args.job_limit);
    let jobs = store
        .list_jobs(&job_filters)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|job| serde_json::to_value(job).ok())
        .map(|value| redactor.redact_json(value))
        .collect();

    let mut event_filters = EventLogFilters::default();
    event_filters.limit = Some(args.event_limit);
    let events = store
        .list_event_log(&event_filters)
        .unwrap_or_default()
        .into_iter()
        .filter(|event| event_matches_session(event, args.session.as_deref()))
        .filter_map(|event| serde_json::to_value(event).ok())
        .map(|value| redactor.redact_json(value))
        .collect();

    let health_checks = store
        .latest_health_checks()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|check| serde_json::to_value(check).ok())
        .map(|value| redactor.redact_json(value))
        .collect();

    StorageCapture::Available {
        table_row_counts,
        jobs,
        events,
        health_checks,
    }
}

fn event_matches_session(
    event: &voom_domain::storage::EventLogRecord,
    session: Option<&str>,
) -> bool {
    let Some(session) = session else {
        return true;
    };
    event.payload.contains(session) || event.summary.contains(session)
}

fn include_env_key(key: &str) -> bool {
    key.starts_with("VOOM_") || key == "RUST_LOG" || key.ends_with("_PATH")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_policy_file_redacts_contents() {
        let dir = tempfile::tempdir().unwrap();
        let policy = dir.path().join("movie-policy.voom");
        std::fs::write(
            &policy,
            "rule test { when file.name == \"The Movie (2026).mkv\" }",
        )
        .unwrap();

        let mut redactor = Redactor::default();
        let captured = collect_policy(&policy, &mut redactor).unwrap();

        assert!(captured.contents.contains("video000.mkv"));
        assert!(!captured.contents.contains("The Movie"));
    }

    #[test]
    fn collect_environment_excludes_unrelated_env_values() {
        let mut redactor = Redactor::default();
        let captured = collect_environment(&mut redactor);

        assert!(!captured.product_version.is_empty());
        assert!(!captured.os.is_empty());
        assert!(
            captured
                .env
                .keys()
                .all(|k| k.starts_with("VOOM_") || k == "RUST_LOG" || k.ends_with("_PATH"))
        );
    }
}
