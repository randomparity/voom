use anyhow::{bail, Result};
use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
use console::style;
use voom_domain::storage::HealthCheckFilters;
use voom_ffmpeg_executor::hwaccel::{resolve_hw_config, HwAccelBackend};
use voom_ffmpeg_executor::probe::{
    enumerate_gpus, probe_hw_capabilities, validate_hw_encoder, validate_hw_encoder_on_device,
    validate_hw_encoders_parallel_with_status, GpuDevice, HwCapabilities,
};

use crate::app;
use crate::cli::{EnvCommands, OutputFormat};
use crate::config;
use crate::output::sanitize_for_display;
use crate::tools::print_tool_status;

mod retention_coverage {
    use chrono::{DateTime, Duration, Utc};

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
}

/// Dispatch environment diagnostic subcommands.
pub fn run(cmd: EnvCommands) -> Result<()> {
    match cmd {
        EnvCommands::Check => check(),
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
#[allow(clippy::unnecessary_wraps)]
pub fn check() -> Result<()> {
    println!("{}", style("VOOM Environment Check").bold().underlined());
    println!();

    let mut issues = 0u32;

    // 1. Config
    print!("  Config file ... ");
    let config_path = config::config_path();
    if config_path.exists() {
        match config::load_config() {
            Ok(_) => println!("{}", style("OK").green()),
            Err(e) => {
                println!("{} {e}", style("ERROR").red());
                issues += 1;
            }
        }
    } else {
        println!("{} (using defaults)", style("not found").yellow());
    }

    // 2. Database
    print!("  Database ... ");
    let config = config::load_config().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to load config, using defaults");
        config::AppConfig::default()
    });
    let kernel_result = app::bootstrap_kernel_with_store(&config);
    match &kernel_result {
        Ok(app::BootstrapResult { store, .. }) => {
            let mut doctor_filters = voom_domain::FileFilters::default();
            doctor_filters.limit = Some(1);
            match store.list_files(&doctor_filters) {
                Ok(_) => println!("{}", style("OK").green()),
                Err(e) => {
                    println!("{} {e}", style("ERROR").red());
                    issues += 1;
                }
            }
        }
        Err(e) => {
            println!("{} {e}", style("ERROR").red());
            issues += 1;
        }
    }

    // 2b. Retention coverage
    print!("  Retention coverage ... ");
    if let Ok(app::BootstrapResult { store, .. }) = &kernel_result {
        let oldest_job = store.oldest_job_created_at().ok().flatten();
        let oldest_event = store.oldest_event_at().ok().flatten();
        match retention_coverage::evaluate(oldest_job, oldest_event) {
            retention_coverage::CoverageStatus::Ok => {
                println!("{}", style("OK").green());
            }
            retention_coverage::CoverageStatus::EventLogEmptyButJobsExist => {
                println!(
                    "{} jobs table is non-empty but event_log is empty — \
                     historical activity queries will be incomplete. \
                     Check [retention.event_log] in config.toml.",
                    style("WARN").yellow()
                );
                issues += 1;
            }
            retention_coverage::CoverageStatus::AsymmetryDetected { gap_seconds } => {
                let hours = gap_seconds / 3600;
                let unit = if hours == 1 { "hour" } else { "hours" };
                println!(
                    "{} oldest event is {} {} newer than the oldest job. \
                     event_log retention is pruning events faster than jobs \
                     are pruned, so `voom events` and SSE history will \
                     undercount completed work. See issue #194.",
                    style("WARN").yellow(),
                    hours,
                    unit
                );
                issues += 1;
            }
        }
    } else {
        println!("{} (database unavailable)", style("skipped").dim());
    }

    // 3. External tools
    println!();
    println!("{}", style("External tools:").bold());

    let mut detector = voom_tool_detector::ToolDetectorPlugin::new();
    detector.detect_all();

    let tool_result = print_tool_status(&detector);
    issues += tool_result.missing_required;

    // 4. Hardware acceleration (only if ffmpeg was found)
    if let Some(ffmpeg_tool) = detector.tool("ffmpeg") {
        print_hw_accel_status(&config, &ffmpeg_tool.path);
    }

    // 5. Plugins
    println!();
    println!("{}", style("Plugins:").bold());
    if let Ok(app::BootstrapResult { kernel, .. }) = &kernel_result {
        let names = kernel.registry.plugin_names();
        println!("  {} plugins registered", style(names.len()).green());
        for name in &names {
            println!("    - {name}");
        }
    }

    // Summary
    println!();
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
    let since_dt = since.map(|s| parse_datetime(&s)).transpose()?;

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

fn parse_datetime(s: &str) -> Result<chrono::DateTime<Utc>> {
    // Try full datetime first: 2024-01-15T10:30:00
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Ok(Utc.from_utc_datetime(&ndt));
    }
    // Try date only: 2024-01-15 (midnight UTC)
    if let Ok(nd) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let ndt = nd.and_hms_opt(0, 0, 0).expect("midnight is always valid");
        return Ok(Utc.from_utc_datetime(&ndt));
    }
    bail!("invalid datetime '{s}': expected YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_detector_creation() {
        let detector = voom_tool_detector::ToolDetectorPlugin::new();
        assert!(detector.tool("nonexistent-tool").is_none());
    }

    #[test]
    fn test_parse_datetime_date_only() {
        let dt = parse_datetime("2024-01-15").unwrap();
        assert_eq!(dt.to_rfc3339(), "2024-01-15T00:00:00+00:00");
    }

    #[test]
    fn test_parse_datetime_full() {
        let dt = parse_datetime("2024-01-15T10:30:00").unwrap();
        assert_eq!(dt.to_rfc3339(), "2024-01-15T10:30:00+00:00");
    }

    #[test]
    fn test_parse_datetime_invalid() {
        assert!(parse_datetime("not-a-date").is_err());
        assert!(parse_datetime("2024/01/15").is_err());
    }
}

#[cfg(test)]
mod retention_coverage_tests {
    use super::retention_coverage::{evaluate, CoverageStatus};
    use chrono::{Duration, Utc};

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
}
