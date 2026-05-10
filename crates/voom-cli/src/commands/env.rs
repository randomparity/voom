use anyhow::Result;
use console::style;
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;
use voom_backup_manager::destination::{BackupDestinationConfig, DestinationKind};
use voom_domain::storage::{HealthCheckFilters, HealthCheckRecord};
use voom_ffmpeg_executor::hwaccel::{HwAccelBackend, resolve_hw_config};
use voom_ffmpeg_executor::probe::{
    GpuDevice, HwCapabilities, enumerate_gpus, probe_hw_capabilities, validate_hw_encoder,
    validate_hw_encoder_on_device, validate_hw_encoders_parallel_with_status,
};

use crate::app;
use crate::cli::{EnvCommands, OutputFormat};
use crate::commands::backup::backup_config_from_app_config;
use crate::config;
use crate::output::sanitize_for_display;
use crate::tools::print_tool_status;
use voom_domain::utils::since::parse_since;

mod retention_coverage {
    use chrono::{DateTime, Duration, Utc};
    use voom_domain::errors::Result;

    /// Hard floor below which we treat a positive lag as noise: the two
    /// `MIN(created_at)` queries that feed `evaluate` are not atomic, and a
    /// short pruning burst can briefly leave the event log starting after the
    /// oldest job by minutes. Anything strictly greater than this floor is
    /// reported as an asymmetry. Operators who want a stricter check can lower
    /// the threshold; doing so trades fewer false negatives for more false
    /// positives during normal retention runs.
    const NOISE_FLOOR: Duration = Duration::hours(1);

    #[derive(Debug, PartialEq, Eq)]
    pub enum CoverageStatus {
        /// No jobs to under-cover (table empty, regardless of event_log),
        /// or events comfortably cover the jobs.
        Ok,
        /// Jobs exist but the event_log is empty.
        EventLogEmptyButJobsExist,
        /// Oldest event is `gap_seconds` newer than the oldest job — events
        /// were pruned while jobs survived.
        AsymmetryDetected { gap_seconds: i64 },
    }

    /// Pure decision function. `oldest_job` is `Some` if any job exists;
    /// `oldest_event` is `Some` if any event_log row exists.
    pub fn evaluate(
        oldest_job: Option<DateTime<Utc>>,
        oldest_event: Option<DateTime<Utc>>,
    ) -> CoverageStatus {
        match (oldest_job, oldest_event) {
            (None, _) => CoverageStatus::Ok,
            (Some(_), None) => CoverageStatus::EventLogEmptyButJobsExist,
            (Some(j), Some(e)) => {
                let lag = e.signed_duration_since(j);
                if lag <= NOISE_FLOOR {
                    CoverageStatus::Ok
                } else {
                    CoverageStatus::AsymmetryDetected {
                        gap_seconds: lag.num_seconds(),
                    }
                }
            }
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    pub struct QueryFailure {
        pub query: &'static str,
        pub error: String,
    }

    pub fn evaluate_query_results(
        oldest_job: Result<Option<DateTime<Utc>>>,
        oldest_event: Result<Option<DateTime<Utc>>>,
    ) -> std::result::Result<CoverageStatus, Vec<QueryFailure>> {
        let mut failures = Vec::new();

        let oldest_job = match oldest_job {
            Ok(value) => value,
            Err(e) => {
                failures.push(QueryFailure {
                    query: "oldest_job_created_at",
                    error: e.to_string(),
                });
                None
            }
        };
        let oldest_event = match oldest_event {
            Ok(value) => value,
            Err(e) => {
                failures.push(QueryFailure {
                    query: "oldest_event_at",
                    error: e.to_string(),
                });
                None
            }
        };

        if failures.is_empty() {
            Ok(evaluate(oldest_job, oldest_event))
        } else {
            Err(failures)
        }
    }
}

/// Dispatch environment diagnostic subcommands.
pub fn run(cmd: EnvCommands) -> Result<()> {
    match cmd {
        EnvCommands::Check { format, json } => {
            let format = if json { OutputFormat::Json } else { format };
            check(format)
        }
        EnvCommands::History {
            check,
            since,
            limit,
            format,
        } => history(check, since, limit, format),
    }
}

/// Print a compatibility warning for the old `voom health ...` command group.
pub fn warn_health_deprecated() {
    eprintln!("warning: `voom health` is deprecated; use `voom env` instead");
}

/// Print a compatibility warning for the old `voom doctor` command.
pub fn warn_doctor_deprecated() {
    eprintln!("warning: `voom doctor` is deprecated; use `voom env check` instead");
}

/// Run live environment checks.
///
/// Tool detection creates a standalone `ToolDetectorPlugin` instance rather
/// than retrieving the kernel-registered one. This is intentional: doctor
/// must be able to diagnose tool availability even when the kernel fails to
/// bootstrap (e.g. missing database directory). The standalone instance does
/// not receive per-plugin configuration from config.toml, but tool-detector
/// currently has no configurable settings.
// Return type mirrors the other subcommand handlers so `main`'s match arms
// all return `Result<()>`; the health check itself never propagates errors.
pub fn check(format: OutputFormat) -> Result<()> {
    let print_human = !matches!(format, OutputFormat::Json);
    if print_human {
        println!("{}", style("VOOM Environment Check").bold().underlined());
        println!();
    }

    let mut issues = 0u32;

    // 1. Config
    if print_human {
        print!("  Config file ... ");
    }
    let config_path = config::config_path();
    let mut config_ok = true;
    if config_path.exists() {
        match config::load_config() {
            Ok(_) => {
                if print_human {
                    println!("{}", style("OK").green());
                }
            }
            Err(e) => {
                if print_human {
                    println!("{} {e}", style("ERROR").red());
                }
                config_ok = false;
                issues += 1;
            }
        }
    } else if print_human {
        println!("{} (using defaults)", style("not found").yellow());
    } else {
        config_ok = true;
    }

    // 2. Database
    if print_human {
        print!("  Database ... ");
    }
    let config = config::load_config().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to load config, using defaults");
        config::AppConfig::default()
    });
    let kernel_result = app::bootstrap_kernel_with_store(&config);
    let mut database_ok = true;
    match &kernel_result {
        Ok(app::BootstrapResult { store, .. }) => {
            let mut doctor_filters = voom_domain::FileFilters::default();
            doctor_filters.limit = Some(1);
            match store.list_files(&doctor_filters) {
                Ok(_) => {
                    if print_human {
                        println!("{}", style("OK").green());
                    }
                }
                Err(e) => {
                    if print_human {
                        println!("{} {e}", style("ERROR").red());
                    }
                    database_ok = false;
                    issues += 1;
                }
            }
        }
        Err(e) => {
            if print_human {
                println!("{} {e}", style("ERROR").red());
            }
            database_ok = false;
            issues += 1;
        }
    }

    // 2b. Retention coverage
    if print_human {
        print!("  Retention coverage ... ");
    }
    if let Ok(app::BootstrapResult { store, .. }) = &kernel_result {
        match retention_coverage::evaluate_query_results(
            store.oldest_job_created_at(),
            store.oldest_event_at(),
        ) {
            Ok(retention_coverage::CoverageStatus::Ok) => {
                if print_human {
                    println!("{}", style("OK").green());
                }
            }
            Ok(retention_coverage::CoverageStatus::EventLogEmptyButJobsExist) => {
                if print_human {
                    println!(
                        "{} jobs table is non-empty but event_log is empty — \
                         historical activity queries will be incomplete. \
                         Check [retention.event_log] in config.toml.",
                        style("WARN").yellow()
                    );
                }
                issues += 1;
            }
            Ok(retention_coverage::CoverageStatus::AsymmetryDetected { gap_seconds }) => {
                let hours = gap_seconds / 3600;
                let unit = if hours == 1 { "hour" } else { "hours" };
                if print_human {
                    println!(
                        "{} oldest event is {} {} newer than the oldest job. \
                         event_log retention is pruning events faster than jobs \
                         are pruned, so `voom events` and SSE history will \
                         undercount completed work. See issue #194.",
                        style("WARN").yellow(),
                        hours,
                        unit
                    );
                }
                issues += 1;
            }
            Err(failures) => {
                if print_human {
                    println!(
                        "{} failed to query retention coverage metadata:",
                        style("ERROR").red()
                    );
                }
                for failure in failures {
                    if print_human {
                        println!("    {}: {}", failure.query, failure.error);
                    }
                    issues += 1;
                }
            }
        }
    } else if print_human {
        println!("{} (database unavailable)", style("skipped").dim());
    }

    // 3. External tools
    if print_human {
        println!();
        println!("{}", style("External tools:").bold());
    }

    let mut detector = voom_tool_detector::ToolDetectorPlugin::new();
    detector.detect_all();

    if print_human {
        let tool_result = print_tool_status(&detector);
        issues += tool_result.missing_required;
    } else {
        for tool in ["ffprobe", "ffmpeg", "mkvmerge", "mkvpropedit"] {
            if detector.tool(tool).is_none() {
                issues += 1;
            }
        }
    }

    let libvmaf = detector
        .tool("ffmpeg")
        .map(|tool| probe_libvmaf(&tool.path))
        .unwrap_or_else(|| LibvmafReport::from_probe(false, None));
    if print_human {
        print_libvmaf_status(&libvmaf);
    }
    if libvmaf.supported && matches!(libvmaf.model_status, VmafModelStatus::Missing) {
        issues += 1;
    }

    // 4. Hardware acceleration (only if ffmpeg was found)
    if let (true, Some(ffmpeg_tool)) = (print_human, detector.tool("ffmpeg")) {
        print_hw_accel_status(&config, &ffmpeg_tool.path);
    }

    // 5. Backup destinations
    let backup_destination_checks = backup_destination_health_checks(&config);
    if print_human && !backup_destination_checks.is_empty() {
        print_backup_destination_health(&backup_destination_checks);
    }
    issues += backup_destination_checks
        .iter()
        .filter(|check| !check.passed)
        .count() as u32;

    // 6. Plugins
    if print_human {
        println!();
        println!("{}", style("Plugins:").bold());
    }
    if let (true, Ok(app::BootstrapResult { kernel, .. })) = (print_human, &kernel_result) {
        let names = kernel.registry.plugin_names();
        println!("  {} plugins registered", style(names.len()).green());
        for name in &names {
            println!("    - {name}");
        }
    }

    let env_passed = config_ok && database_ok && issues == 0;
    if let Ok(app::BootstrapResult { store, .. }) = &kernel_result {
        for check in &backup_destination_checks {
            let record =
                HealthCheckRecord::new(&check.check_name, check.passed, check.details.clone());
            if let Err(e) = store.insert_health_check(&record) {
                tracing::warn!(
                    check_name = %check.check_name,
                    error = %e,
                    "failed to persist backup destination health check"
                );
            }
        }
        let record = env_snapshot_record(&libvmaf, env_passed, &backup_destination_checks);
        if let Err(e) = store.insert_health_check(&record) {
            tracing::warn!(error = %e, "failed to persist env check snapshot");
        }
    }

    if matches!(format, OutputFormat::Json) {
        let value = env_snapshot_json(&libvmaf, env_passed, issues, &backup_destination_checks);
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    // Summary
    if print_human {
        println!();
    }
    if issues == 0 {
        println!("{}", style("All checks passed.").bold().green());
    } else {
        println!(
            "{} {} issue(s) found.",
            style("WARNING").bold().yellow(),
            issues
        );
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BackupDestinationHealthCheck {
    check_name: String,
    destination_name: String,
    kind: DestinationKind,
    passed: bool,
    status: BackupDestinationHealthStatus,
    details: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackupDestinationHealthStatus {
    Healthy,
    ConfigInvalid,
    RcloneUnavailable,
    RemoteUnreachable,
    ProbeFailed,
}

impl BackupDestinationHealthStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::ConfigInvalid => "config_invalid",
            Self::RcloneUnavailable => "rclone_unavailable",
            Self::RemoteUnreachable => "remote_unreachable",
            Self::ProbeFailed => "probe_failed",
        }
    }
}

fn backup_destination_health_checks(
    config: &config::AppConfig,
) -> Vec<BackupDestinationHealthCheck> {
    backup_destination_health_checks_with_runner(config, |program, args| {
        run_backup_destination_probe_command(program, args)
    })
}

fn backup_destination_health_checks_with_runner(
    config: &config::AppConfig,
    mut runner: impl FnMut(&str, &[String]) -> Result<()>,
) -> Vec<BackupDestinationHealthCheck> {
    let backup_config = match backup_config_from_app_config(config) {
        Ok(config) => config,
        Err(e) => {
            return vec![backup_destination_config_failure(e.to_string())];
        }
    };
    backup_config
        .destinations
        .iter()
        .map(|destination| {
            check_backup_destination(
                destination,
                &backup_config.rclone_path,
                &config.data_dir,
                &mut runner,
            )
        })
        .collect()
}

fn backup_destination_config_failure(error: String) -> BackupDestinationHealthCheck {
    BackupDestinationHealthCheck {
        check_name: "backup_destinations_config".to_string(),
        destination_name: "backup-manager".to_string(),
        kind: DestinationKind::Local,
        passed: false,
        status: BackupDestinationHealthStatus::ConfigInvalid,
        details: Some(format!("invalid backup destination configuration: {error}")),
    }
}

fn check_backup_destination(
    destination: &BackupDestinationConfig,
    rclone_path: &str,
    data_dir: &Path,
    runner: &mut impl FnMut(&str, &[String]) -> Result<()>,
) -> BackupDestinationHealthCheck {
    let check_name = format!("backup_destination:{}", destination.name);
    if !destination.kind.is_rclone_backed() {
        return BackupDestinationHealthCheck {
            check_name,
            destination_name: destination.name.clone(),
            kind: destination.kind,
            passed: true,
            status: BackupDestinationHealthStatus::Healthy,
            details: Some("local backup destination does not require rclone".to_string()),
        };
    }
    let Some(remote) = destination.remote.as_deref() else {
        return backup_destination_failure(
            destination,
            check_name,
            BackupDestinationHealthStatus::ConfigInvalid,
            "remote is required for rclone-backed backup destination",
        );
    };
    run_rclone_destination_checks(
        destination,
        check_name,
        rclone_path,
        remote,
        data_dir,
        runner,
    )
}

fn run_rclone_destination_checks(
    destination: &BackupDestinationConfig,
    check_name: String,
    rclone_path: &str,
    remote: &str,
    data_dir: &Path,
    runner: &mut impl FnMut(&str, &[String]) -> Result<()>,
) -> BackupDestinationHealthCheck {
    if runner(rclone_path, &[String::from("version")]).is_err() {
        return backup_destination_failure(
            destination,
            check_name,
            BackupDestinationHealthStatus::RcloneUnavailable,
            "rclone is unavailable for this backup destination",
        );
    }
    if runner(
        rclone_path,
        &[
            String::from("lsf"),
            remote.to_string(),
            String::from("--max-depth"),
            String::from("1"),
        ],
    )
    .is_err()
    {
        return backup_destination_failure(
            destination,
            check_name,
            BackupDestinationHealthStatus::RemoteUnreachable,
            "remote is unreachable for this backup destination",
        );
    }
    run_backup_destination_write_probe(
        destination,
        check_name,
        rclone_path,
        remote,
        data_dir,
        runner,
    )
}

fn run_backup_destination_write_probe(
    destination: &BackupDestinationConfig,
    check_name: String,
    rclone_path: &str,
    remote: &str,
    data_dir: &Path,
    runner: &mut impl FnMut(&str, &[String]) -> Result<()>,
) -> BackupDestinationHealthCheck {
    let probe_id = Uuid::new_v4();
    let local_probe = data_dir.join(format!(".voom-backup-destination-health-{probe_id}.tmp"));
    if let Err(e) = std::fs::write(&local_probe, b"voom backup destination health probe\n") {
        return backup_destination_failure(
            destination,
            check_name,
            BackupDestinationHealthStatus::ProbeFailed,
            &format!("failed to create local probe file: {e}"),
        );
    }
    let remote_probe = format!(
        "{}/.voom-health/{probe_id}.tmp",
        remote.trim_end_matches('/')
    );
    let copy_result = runner(
        rclone_path,
        &[
            String::from("copyto"),
            local_probe.display().to_string(),
            remote_probe.clone(),
        ],
    );
    let delete_result = if copy_result.is_ok() {
        runner(rclone_path, &[String::from("deletefile"), remote_probe])
    } else {
        Ok(())
    };
    let _ = std::fs::remove_file(&local_probe);
    if copy_result.is_err() || delete_result.is_err() {
        return backup_destination_failure(
            destination,
            check_name,
            BackupDestinationHealthStatus::ProbeFailed,
            "write/delete probe failed for this backup destination",
        );
    }
    BackupDestinationHealthCheck {
        check_name,
        destination_name: destination.name.clone(),
        kind: destination.kind,
        passed: true,
        status: BackupDestinationHealthStatus::Healthy,
        details: Some("remote reachable; write/delete probe succeeded".to_string()),
    }
}

fn backup_destination_failure(
    destination: &BackupDestinationConfig,
    check_name: String,
    status: BackupDestinationHealthStatus,
    details: &str,
) -> BackupDestinationHealthCheck {
    BackupDestinationHealthCheck {
        check_name,
        destination_name: destination.name.clone(),
        kind: destination.kind,
        passed: false,
        status,
        details: Some(details.to_string()),
    }
}

fn run_backup_destination_probe_command(program: &str, args: &[String]) -> Result<()> {
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = voom_process::run_with_timeout(program, &arg_refs, Duration::from_secs(10))?;
    if output.status.success() {
        Ok(())
    } else {
        anyhow::bail!("backup destination probe command failed")
    }
}

fn print_backup_destination_health(checks: &[BackupDestinationHealthCheck]) {
    println!();
    println!("{}", style("Backup destinations:").bold());
    for check in checks {
        let status = if check.passed {
            style("OK").green()
        } else {
            style("FAIL").red()
        };
        println!(
            "  {} ({}) ... {}",
            sanitize_for_display(&check.destination_name),
            check.kind.as_str(),
            status
        );
        if let Some(details) = &check.details {
            println!("    {}", style(details).dim());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VmafModelStatus {
    Present,
    Missing,
    NotRequired,
}

impl VmafModelStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Missing => "missing",
            Self::NotRequired => "not_required",
        }
    }
}

#[derive(Debug, Clone)]
struct LibvmafReport {
    supported: bool,
    model_dir: Option<PathBuf>,
    model_status: VmafModelStatus,
}

impl LibvmafReport {
    fn from_probe(supported: bool, model_dir: Option<PathBuf>) -> Self {
        let model_status = match (supported, model_dir.is_some()) {
            (true, true) => VmafModelStatus::Present,
            (true, false) => VmafModelStatus::Missing,
            (false, _) => VmafModelStatus::NotRequired,
        };
        Self {
            supported,
            model_dir,
            model_status,
        }
    }
}

fn probe_libvmaf(ffmpeg_path: &Path) -> LibvmafReport {
    let ffmpeg = ffmpeg_path.to_string_lossy();
    let filters = run_ffmpeg_probe(&ffmpeg, &["-hide_banner", "-filters"]);
    let version = if filters
        .as_deref()
        .is_some_and(ffmpeg_output_reports_libvmaf)
    {
        None
    } else {
        run_ffmpeg_probe(&ffmpeg, &["-version"])
    };
    let supported = filters
        .as_deref()
        .is_some_and(ffmpeg_output_reports_libvmaf)
        || version
            .as_deref()
            .is_some_and(ffmpeg_output_reports_libvmaf);
    LibvmafReport::from_probe(supported, resolve_vmaf_model_dir())
}

fn run_ffmpeg_probe(ffmpeg: &str, args: &[&str]) -> Option<String> {
    let output = voom_process::run_with_timeout(ffmpeg, args, Duration::from_secs(5)).ok()?;
    if !output.status.success() {
        return None;
    }
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    Some(combined)
}

fn ffmpeg_output_reports_libvmaf(output: &str) -> bool {
    output.contains("--enable-libvmaf")
        || output
            .lines()
            .any(|line| line.split_whitespace().any(|token| token == "libvmaf"))
}

fn resolve_vmaf_model_dir() -> Option<PathBuf> {
    vmaf_model_candidates()
        .into_iter()
        .find(|candidate| candidate.is_dir())
}

fn vmaf_model_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("/usr/share/model"),
        PathBuf::from("/opt/homebrew/share/libvmaf/model"),
    ];
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".config/voom/vmaf-models"));
    }
    candidates
}

fn print_libvmaf_status(report: &LibvmafReport) {
    print!("  libvmaf: ");
    match (
        report.supported,
        report.model_dir.as_ref(),
        report.model_status,
    ) {
        (true, Some(path), VmafModelStatus::Present) => {
            println!(
                "{} (model dir: {})",
                style("yes").green(),
                sanitize_for_display(&path.display().to_string())
            );
        }
        (true, None, VmafModelStatus::Missing) => {
            println!(
                "{} {}",
                style("yes, model: missing").yellow(),
                style("install VMAF models under ~/.config/voom/vmaf-models").dim()
            );
        }
        (false, _, VmafModelStatus::NotRequired) => {
            println!(
                "{}",
                style("no — VMAF-guided encoding will fall back to CRF").yellow()
            );
        }
        _ => {
            println!("{}", style("unknown").yellow());
        }
    }
}

fn env_snapshot_record(
    report: &LibvmafReport,
    passed: bool,
    backup_checks: &[BackupDestinationHealthCheck],
) -> HealthCheckRecord {
    HealthCheckRecord::new(
        "env_check",
        passed && !matches!(report.model_status, VmafModelStatus::Missing),
        Some(env_snapshot_json(report, passed, 0, backup_checks).to_string()),
    )
}

fn env_snapshot_json(
    report: &LibvmafReport,
    passed: bool,
    issue_count: u32,
    backup_checks: &[BackupDestinationHealthCheck],
) -> serde_json::Value {
    serde_json::json!({
        "passed": passed && !matches!(report.model_status, VmafModelStatus::Missing),
        "issue_count": issue_count,
        "vmaf_supported": report.supported,
        "vmaf_model_dir": report.model_dir.as_ref().map(|p| p.display().to_string()),
        "vmaf_model_status": report.model_status.as_str(),
        "backup_destinations": backup_checks_json(backup_checks),
    })
}

fn backup_checks_json(checks: &[BackupDestinationHealthCheck]) -> Vec<serde_json::Value> {
    checks
        .iter()
        .map(|check| {
            serde_json::json!({
                "check_name": check.check_name,
                "destination_name": check.destination_name,
                "kind": check.kind.as_str(),
                "passed": check.passed,
                "status": check.status.as_str(),
                "details": check.details,
            })
        })
        .collect()
}

fn backend_label(backend: HwAccelBackend) -> &'static str {
    match backend {
        HwAccelBackend::Nvenc => "NVENC (cuda)",
        HwAccelBackend::Qsv => "QuickSync (qsv)",
        HwAccelBackend::Vaapi => "VA-API (vaapi)",
        HwAccelBackend::Videotoolbox => "VideoToolbox",
    }
}

fn gpu_section_header(backend: HwAccelBackend) -> &'static str {
    match backend {
        HwAccelBackend::Nvenc => "GPUs",
        HwAccelBackend::Vaapi | HwAccelBackend::Qsv => "Render devices",
        HwAccelBackend::Videotoolbox => "Devices",
    }
}

fn gpu_display_label(device: &GpuDevice, backend: HwAccelBackend) -> String {
    let name = sanitize_for_display(&device.name);
    match backend {
        HwAccelBackend::Nvenc => {
            let vram = device
                .vram_mib
                .map(|m| format!(" ({m} MiB)"))
                .unwrap_or_default();
            format!("GPU {}: {}{}", device.id, name, vram)
        }
        _ => {
            if device.name == device.id {
                device.id.clone()
            } else {
                format!("{} ({})", device.id, name)
            }
        }
    }
}

fn encoder_block_label(device: &GpuDevice, backend: HwAccelBackend) -> String {
    match backend {
        HwAccelBackend::Nvenc => {
            let name = sanitize_for_display(&device.name);
            format!("GPU {} — {}", device.id, name)
        }
        _ => device.id.clone(),
    }
}

fn print_encoder_block(hw_encoders: &[String], backend: HwAccelBackend, device: &GpuDevice) {
    let label = encoder_block_label(device, backend);
    println!();
    println!("  HW Encoders ({label}):");
    let results = validate_hw_encoders_parallel_with_status(hw_encoders, |enc| {
        validate_hw_encoder_on_device(enc, backend, device)
    });
    for (enc, ok) in results {
        if ok {
            println!("    {:<20}{}", enc, style("OK (device validated)").green());
        } else {
            println!("    {:<20}{}", enc, style("UNSUPPORTED").yellow());
        }
    }
}

/// Resolve and print the HW acceleration backend. Returns `None` if no
/// backend is available (and prints the appropriate status line).
fn print_hw_backend(
    hw_accel_override: Option<&str>,
    hw_accels: &[String],
) -> Option<HwAccelBackend> {
    if !hw_accels.is_empty() {
        println!("  Available ... {}", hw_accels.join(", "));
    }

    let (config, source) = resolve_hw_config(hw_accel_override, hw_accels);

    if let Some(backend) = config.backend {
        println!(
            "  Backend ... {} {}",
            style(backend_label(backend)).green(),
            style(format!("({source})")).dim()
        );
        Some(backend)
    } else {
        if source == "disabled" {
            println!(
                "  Backend ... {} {}",
                style("disabled").yellow(),
                style("(config override)").dim()
            );
        } else {
            println!("  Backend ... {}", style("none detected").yellow());
        }
        None
    }
}

/// Print HW encoder validation results, grouped by device when available.
fn print_hw_encoders(hw_encoders: &[String], devices: &[GpuDevice], backend: HwAccelBackend) {
    if hw_encoders.is_empty() {
        return;
    }
    if devices.is_empty() {
        println!();
        println!("  HW Encoders:");
        for enc in hw_encoders {
            if validate_hw_encoder(enc) {
                println!("    {:<20}{}", enc, style("OK (device validated)").green());
            } else {
                println!("    {:<20}{}", enc, style("UNSUPPORTED").yellow());
            }
        }
    } else {
        for device in devices {
            print_encoder_block(hw_encoders, backend, device);
        }
    }
}

/// Print HW decoder availability.
fn print_hw_decoders(hw_decoders: &[String]) {
    if hw_decoders.is_empty() {
        return;
    }
    println!();
    println!("  HW Decoders:");
    for dec in hw_decoders {
        println!("    {:<20}{}", dec, style("available").green());
    }
}

/// Print the configured GPU device status line.
fn print_configured_gpu(app_config: &config::AppConfig, devices: &[GpuDevice]) {
    if let Some(device_id) = app_config
        .plugin
        .get("ffmpeg-executor")
        .and_then(|t| t.get("gpu_device"))
        .and_then(|v| v.as_str())
    {
        let found = devices.iter().any(|d| d.id == device_id);
        let safe_id = sanitize_for_display(device_id);
        if found {
            println!(
                "  Configured GPU ... {} {}",
                style(&safe_id).cyan(),
                style("(found)").green()
            );
        } else {
            println!(
                "  Configured GPU ... {} {}",
                style(&safe_id).cyan(),
                style("(NOT FOUND)").red()
            );
        }
    }
}

fn print_hw_accel_status(app_config: &config::AppConfig, ffmpeg_path: &std::path::Path) {
    println!();
    println!("{}", style("Hardware acceleration:").bold());

    let ffmpeg = ffmpeg_path.to_string_lossy();
    let HwCapabilities {
        hw_accels,
        encoders,
        decoders,
    } = probe_hw_capabilities(&ffmpeg);

    let hw_accel_override = app_config
        .plugin
        .get("ffmpeg-executor")
        .and_then(|t| t.get("hw_accel"))
        .and_then(|v| v.as_str());

    let Some(backend) = print_hw_backend(hw_accel_override, &hw_accels) else {
        return;
    };

    let devices = enumerate_gpus(backend);

    if !devices.is_empty() {
        println!();
        println!("  {}:", gpu_section_header(backend));
        for device in &devices {
            println!("    {}", gpu_display_label(device, backend));
        }
    }

    print_configured_gpu(app_config, &devices);
    print_hw_encoders(&encoders, &devices, backend);
    print_hw_decoders(&decoders);
}

fn history(
    check_name: Option<String>,
    since: Option<String>,
    limit: u32,
    format: OutputFormat,
) -> Result<()> {
    let since_dt = since.map(|s| parse_since(&s)).transpose()?;

    let mut filters = HealthCheckFilters::default();
    filters.check_name = check_name;
    filters.since = since_dt;
    filters.limit = Some(limit);

    let config = config::load_config().unwrap_or_default();
    let store = app::open_store(&config)?;
    let records = store.list_health_checks(&filters)?;

    match format {
        OutputFormat::Table => {
            if records.is_empty() {
                eprintln!("No environment check records found.");
                return Ok(());
            }
            println!(
                "{:<20} {:<24} {:<8} Details",
                "Timestamp", "Check", "Status"
            );
            println!("{}", "-".repeat(76));
            for r in &records {
                let status = if r.passed {
                    style("PASS").green().to_string()
                } else {
                    style("FAIL").red().to_string()
                };
                let details = r.details.as_deref().unwrap_or("");
                println!(
                    "{:<20} {:<24} {:<8} {}",
                    r.checked_at.format("%Y-%m-%d %H:%M:%S"),
                    r.check_name,
                    status,
                    details,
                );
            }
        }
        OutputFormat::Json => {
            let json: Vec<serde_json::Value> = records
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "checked_at": r.checked_at.to_rfc3339(),
                        "check_name": r.check_name,
                        "passed": r.passed,
                        "details": r.details,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
        OutputFormat::Plain | OutputFormat::Csv => {
            for r in &records {
                let status = if r.passed { "PASS" } else { "FAIL" };
                println!("{}\t{}", r.checked_at.format("%Y-%m-%d %H:%M:%S"), status,);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use anyhow::bail;
    use voom_domain::storage::HealthCheckRecord;

    fn app_config(toml: &str) -> crate::config::AppConfig {
        toml::from_str(toml).expect("app config")
    }

    #[test]
    fn test_tool_detector_creation() {
        let detector = voom_tool_detector::ToolDetectorPlugin::new();
        assert!(detector.tool("nonexistent-tool").is_none());
    }

    #[test]
    fn libvmaf_probe_detects_filter_from_ffmpeg_filters() {
        let output =
            " ... libvmaf           V->V       Calculate the VMAF between two video streams.";

        assert!(super::ffmpeg_output_reports_libvmaf(output));
    }

    #[test]
    fn libvmaf_probe_detects_build_flag_from_ffmpeg_version() {
        let output = "configuration: --enable-gpl --enable-libx264 --enable-libvmaf";

        assert!(super::ffmpeg_output_reports_libvmaf(output));
    }

    #[test]
    fn libvmaf_probe_rejects_absent_output() {
        let output = "configuration: --enable-gpl --enable-libx264\n ... vmafmotion";

        assert!(!super::ffmpeg_output_reports_libvmaf(output));
    }

    #[test]
    fn model_status_reports_missing_model_when_no_candidate_exists() {
        let report = super::LibvmafReport::from_probe(true, None);

        assert!(report.supported);
        assert_eq!(report.model_dir, None);
        assert!(matches!(
            report.model_status,
            super::VmafModelStatus::Missing
        ));
    }

    #[test]
    fn env_snapshot_details_include_vmaf_supported_and_model_dir() {
        let report = super::LibvmafReport::from_probe(
            true,
            Some(PathBuf::from("/opt/homebrew/share/libvmaf/model")),
        );

        let record = super::env_snapshot_record(&report, true, &[]);
        let details = record.details.expect("snapshot details");
        let value: serde_json::Value = serde_json::from_str(&details).expect("json details");

        assert_eq!(record.check_name, "env_check");
        assert!(record.passed);
        assert_eq!(value["vmaf_supported"], true);
        assert_eq!(value["vmaf_model_dir"], "/opt/homebrew/share/libvmaf/model");
    }

    #[test]
    fn env_snapshot_record_fails_when_model_is_missing() {
        let report = super::LibvmafReport::from_probe(true, None);

        let record: HealthCheckRecord = super::env_snapshot_record(&report, true, &[]);

        assert!(!record.passed);
        assert!(
            record
                .details
                .as_deref()
                .unwrap_or_default()
                .contains("\"vmaf_model_status\":\"missing\"")
        );
    }

    #[test]
    fn backup_destination_health_reports_healthy_destination() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = app_config(&format!(
            r#"
data_dir = "{}"

[plugin.backup-manager]
rclone_path = "/bin/rclone"

[[plugin.backup-manager.destinations]]
name = "offsite"
kind = "rclone"
remote = "s3:bucket/secret-path"
"#,
            dir.path().display()
        ));
        let mut commands = Vec::new();

        let checks =
            super::backup_destination_health_checks_with_runner(&config, |_program, args| {
                commands.push(args.to_vec());
                Ok(())
            });

        assert_eq!(checks.len(), 1);
        assert!(checks[0].passed);
        assert_eq!(checks[0].check_name, "backup_destination:offsite");
        assert_eq!(
            checks[0].status,
            super::BackupDestinationHealthStatus::Healthy
        );
        assert!(
            !checks[0]
                .details
                .as_deref()
                .unwrap_or_default()
                .contains("secret-path")
        );
        assert!(commands.iter().any(|args| args[0] == "copyto"));
        assert!(commands.iter().any(|args| args[0] == "deletefile"));
    }

    #[test]
    fn backup_destination_health_reports_missing_rclone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = app_config(&format!(
            r#"
data_dir = "{}"

[plugin.backup-manager]
rclone_path = "/missing/rclone"

[[plugin.backup-manager.destinations]]
name = "offsite"
kind = "rclone"
remote = "s3:bucket"
"#,
            dir.path().display()
        ));

        let checks =
            super::backup_destination_health_checks_with_runner(&config, |_program, args| {
                if args[0] == "version" {
                    bail!("missing rclone");
                }
                Ok(())
            });

        assert_eq!(
            checks[0].status,
            super::BackupDestinationHealthStatus::RcloneUnavailable
        );
        assert!(!checks[0].passed);
    }

    #[test]
    fn backup_destination_health_reports_unreachable_remote() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = app_config(&format!(
            r#"
data_dir = "{}"

[plugin.backup-manager]
rclone_path = "/bin/rclone"

[[plugin.backup-manager.destinations]]
name = "offsite"
kind = "rclone"
remote = "s3:bucket"
"#,
            dir.path().display()
        ));

        let checks =
            super::backup_destination_health_checks_with_runner(&config, |_program, args| {
                if args[0] == "lsf" {
                    bail!("unreachable");
                }
                Ok(())
            });

        assert_eq!(
            checks[0].status,
            super::BackupDestinationHealthStatus::RemoteUnreachable
        );
        assert!(!checks[0].passed);
    }

    #[test]
    fn backup_destination_health_reports_duplicate_destination_names() {
        let config = app_config(
            r#"
[plugin.backup-manager]
rclone_path = "/bin/rclone"

[[plugin.backup-manager.destinations]]
name = "offsite"
kind = "rclone"
remote = "s3:bucket-a"

[[plugin.backup-manager.destinations]]
name = "offsite"
kind = "rclone"
remote = "s3:bucket-b"
"#,
        );

        let checks =
            super::backup_destination_health_checks_with_runner(&config, |_program, _args| Ok(()));

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].check_name, "backup_destinations_config");
        assert_eq!(
            checks[0].status,
            super::BackupDestinationHealthStatus::ConfigInvalid
        );
        assert!(!checks[0].passed);
        assert!(
            checks[0]
                .details
                .as_deref()
                .unwrap_or_default()
                .contains("duplicate backup destination")
        );
    }
}

#[cfg(test)]
mod retention_coverage_tests {
    use super::retention_coverage::{CoverageStatus, evaluate, evaluate_query_results};
    use chrono::{Duration, Utc};
    use voom_domain::errors::{StorageErrorKind, VoomError};

    #[test]
    fn ok_when_no_jobs_and_no_events() {
        assert_eq!(evaluate(None, None), CoverageStatus::Ok);
    }

    #[test]
    fn ok_when_oldest_event_predates_oldest_job() {
        let now = Utc::now();
        let oldest_job = Some(now - Duration::hours(2));
        let oldest_event = Some(now - Duration::hours(3));
        assert_eq!(evaluate(oldest_job, oldest_event), CoverageStatus::Ok);
    }

    #[test]
    fn ok_when_event_only_slightly_newer_than_oldest_job() {
        let now = Utc::now();
        let oldest_job = Some(now - Duration::hours(48));
        let oldest_event = Some(now - Duration::hours(48) + Duration::minutes(5));
        assert_eq!(evaluate(oldest_job, oldest_event), CoverageStatus::Ok);
    }

    #[test]
    fn warn_when_event_log_starts_well_after_oldest_job() {
        let now = Utc::now();
        let oldest_job = Some(now - Duration::days(7));
        let oldest_event = Some(now - Duration::hours(1));
        match evaluate(oldest_job, oldest_event) {
            CoverageStatus::AsymmetryDetected { gap_seconds } => {
                assert!(gap_seconds >= 6 * 24 * 3600);
            }
            other => panic!("expected AsymmetryDetected, got {other:?}"),
        }
    }

    #[test]
    fn warn_when_jobs_present_but_event_log_empty() {
        let now = Utc::now();
        let oldest_job = Some(now - Duration::days(1));
        match evaluate(oldest_job, None) {
            CoverageStatus::EventLogEmptyButJobsExist => {}
            other => panic!("expected EventLogEmptyButJobsExist, got {other:?}"),
        }
    }

    #[test]
    fn ok_when_lag_equals_noise_floor() {
        let oldest_job = chrono::Utc::now() - chrono::Duration::days(1);
        let oldest_event = oldest_job + chrono::Duration::hours(1);
        assert_eq!(
            evaluate(Some(oldest_job), Some(oldest_event)),
            CoverageStatus::Ok
        );
    }

    #[test]
    fn warn_when_lag_exceeds_noise_floor_by_one_second() {
        let oldest_job = chrono::Utc::now() - chrono::Duration::days(1);
        let oldest_event = oldest_job + chrono::Duration::hours(1) + chrono::Duration::seconds(1);
        match evaluate(Some(oldest_job), Some(oldest_event)) {
            CoverageStatus::AsymmetryDetected { gap_seconds } => {
                assert_eq!(gap_seconds, 3601);
            }
            other => panic!("expected AsymmetryDetected, got {other:?}"),
        }
    }

    #[test]
    fn query_failures_are_reported_without_evaluation() {
        let failed_job_query = Err(VoomError::Storage {
            kind: StorageErrorKind::Other,
            message: "job query failed".into(),
        });
        let failed_event_query = Err(VoomError::Storage {
            kind: StorageErrorKind::Other,
            message: "event query failed".into(),
        });

        let failures = evaluate_query_results(failed_job_query, failed_event_query).unwrap_err();

        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0].query, "oldest_job_created_at");
        assert!(failures[0].error.contains("job query failed"));
        assert_eq!(failures[1].query, "oldest_event_at");
        assert!(failures[1].error.contains("event query failed"));
    }
}
