//! `voom verify run` and `voom verify report` command implementations.
//!
//! The CLI calls the verifier's library functions directly (not via the
//! event bus). The bus path is reserved for DSL-driven phase plans.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use console::style;
use rayon::prelude::*;

use voom_domain::storage::{FileFilters, StorageTrait};
use voom_domain::verification::{
    VerificationFilters, VerificationMode, VerificationOutcome, VerificationRecord,
};
use voom_verifier::hwaccel::{self as verifier_hwaccel, HwAccelMode};
use voom_verifier::VerifierConfig;

use crate::app;
use crate::cli::{HwAccelArg, OutputFormat, VerifyArgs, VerifyCommands, VerifyReportArgs};
use crate::config::{self, AppConfig};
use voom_domain::utils::since::parse_since;

/// Top-level dispatcher for `voom verify <subcommand>`.
///
/// # Errors
/// Returns errors from configuration loading, storage access, or
/// verification tool execution.
pub fn run(cmd: VerifyCommands) -> Result<()> {
    match cmd {
        VerifyCommands::Run(args) => run_verify(args),
        VerifyCommands::Report(args) => run_report(args),
    }
}

fn run_verify(args: VerifyArgs) -> Result<()> {
    let mode = if args.thorough {
        VerificationMode::Thorough
    } else if args.hash {
        VerificationMode::Hash
    } else {
        VerificationMode::Quick
    };

    let cfg = config::load_config().unwrap_or_default();
    let store = app::open_store(&cfg)?;
    let verifier_cfg = read_verifier_config(&cfg);

    let targets = resolve_targets(&store, &args)?;
    if targets.is_empty() {
        eprintln!("No files to verify.");
        return Ok(());
    }

    let workers = if args.workers == 0 {
        std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(4)
    } else {
        args.workers
    };

    let mut records: Vec<VerificationRecord> = Vec::with_capacity(targets.len());

    match mode {
        VerificationMode::Thorough => {
            let hw_accel = resolve_thorough_hw_accel(&verifier_cfg, args.hw_accel);
            // Serial: ffmpeg saturates cores already.
            for tgt in &targets {
                let timeout = compute_thorough_timeout(&verifier_cfg, tgt.duration);
                let rec = voom_verifier::thorough::run_thorough(
                    &tgt.file_id,
                    &tgt.path,
                    &verifier_cfg.ffmpeg_path,
                    timeout,
                    hw_accel,
                )?;
                store.insert_verification(&rec)?;
                print_record(&rec, &tgt.path);
                records.push(rec);
            }
        }
        VerificationMode::Quick | VerificationMode::Hash => {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(workers)
                .build()
                .context("failed to build rayon pool")?;
            let parallel: Vec<Result<(VerificationRecord, PathBuf)>> = pool.install(|| {
                targets
                    .par_iter()
                    .map(|tgt| {
                        run_one(&store, &verifier_cfg, mode, tgt).map(|rec| (rec, tgt.path.clone()))
                    })
                    .collect()
            });
            for r in parallel {
                let (rec, path) = r?;
                print_record(&rec, &path);
                records.push(rec);
            }
        }
    }

    print_summary(&records);
    let any_errors = records
        .iter()
        .any(|r| r.outcome == VerificationOutcome::Error);
    if any_errors {
        std::process::exit(1);
    }
    Ok(())
}

struct VerifyTarget {
    file_id: String,
    path: PathBuf,
    duration: Option<f64>,
}

fn resolve_targets(store: &Arc<dyn StorageTrait>, args: &VerifyArgs) -> Result<Vec<VerifyTarget>> {
    if !args.paths.is_empty() {
        return resolve_explicit_paths(store, &args.paths);
    }
    resolve_due_targets(store, args)
}

fn resolve_explicit_paths(
    store: &Arc<dyn StorageTrait>,
    paths: &[PathBuf],
) -> Result<Vec<VerifyTarget>> {
    let mut out = Vec::new();
    for p in paths {
        let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
        if let Some(f) = store.file_by_path(&canon)? {
            out.push(VerifyTarget {
                file_id: f.id.to_string(),
                path: f.path.clone(),
                duration: Some(f.duration),
            });
        }
    }
    if out.is_empty() {
        bail!("none of the given paths match files in the database; run `voom scan` first");
    }
    Ok(out)
}

fn resolve_due_targets(
    store: &Arc<dyn StorageTrait>,
    args: &VerifyArgs,
) -> Result<Vec<VerifyTarget>> {
    let cutoff = if args.all {
        DateTime::<Utc>::UNIX_EPOCH
    } else {
        parse_since(&args.since).context("parsing --since")?
    };

    let files = store.list_files(&FileFilters::default())?;
    let mut out = Vec::new();
    for f in files {
        let mut latest_filters = VerificationFilters::default();
        latest_filters.file_id = Some(f.id.to_string());
        latest_filters.limit = Some(1);
        let latest = store.list_verifications(&latest_filters)?;
        let needs = match latest.first() {
            Some(rec) => rec.verified_at < cutoff,
            None => true,
        };
        if needs {
            out.push(VerifyTarget {
                file_id: f.id.to_string(),
                path: f.path.clone(),
                duration: Some(f.duration),
            });
        }
    }
    Ok(out)
}

/// Minimal input for a parallel quick-verify pass.
pub(crate) struct QuickVerifyTarget {
    pub file_id: String,
    pub path: PathBuf,
}

/// Run quick-mode verification on `targets` in parallel using a rayon pool,
/// persisting each record to `store`. Errors from individual files are
/// logged via `tracing` and reported as `VerificationOutcome::Error` — the
/// pass itself does not abort on per-file failures.
pub(crate) fn run_quick_pass(
    store: &Arc<dyn StorageTrait>,
    verifier_cfg: &VerifierConfig,
    targets: &[QuickVerifyTarget],
    workers: usize,
) -> Result<Vec<VerificationRecord>> {
    if targets.is_empty() {
        return Ok(Vec::new());
    }
    let workers = if workers == 0 {
        std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(4)
    } else {
        workers
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()
        .context("failed to build rayon pool")?;
    let timeout = Duration::from_secs(verifier_cfg.quick_timeout_secs);
    let ffprobe_path = verifier_cfg.ffprobe_path.clone();
    let results: Vec<Result<VerificationRecord>> = pool.install(|| {
        targets
            .par_iter()
            .map(|tgt| {
                voom_verifier::quick::run_quick(&tgt.file_id, &tgt.path, &ffprobe_path, timeout)
                    .map_err(anyhow::Error::from)
            })
            .collect()
    });

    let mut records = Vec::with_capacity(results.len());
    for r in results {
        match r {
            Ok(rec) => {
                if let Err(e) = store.insert_verification(&rec) {
                    tracing::warn!(
                        file_id = %rec.file_id,
                        error = %e,
                        "failed to persist verification record"
                    );
                }
                records.push(rec);
            }
            Err(e) => {
                tracing::warn!(error = %e, "quick verify failed");
            }
        }
    }
    Ok(records)
}

fn run_one(
    store: &Arc<dyn StorageTrait>,
    verifier_cfg: &VerifierConfig,
    mode: VerificationMode,
    tgt: &VerifyTarget,
) -> Result<VerificationRecord> {
    let rec = match mode {
        VerificationMode::Quick => voom_verifier::quick::run_quick(
            &tgt.file_id,
            &tgt.path,
            &verifier_cfg.ffprobe_path,
            Duration::from_secs(verifier_cfg.quick_timeout_secs),
        )?,
        VerificationMode::Hash => {
            let prior = store.latest_verification(&tgt.file_id, VerificationMode::Hash)?;
            voom_verifier::hash::run_hash(&tgt.file_id, &tgt.path, prior.as_ref())?
        }
        VerificationMode::Thorough => unreachable!("thorough goes through serial path"),
    };
    store.insert_verification(&rec)?;
    Ok(rec)
}

fn compute_thorough_timeout(cfg: &VerifierConfig, duration: Option<f64>) -> Duration {
    voom_verifier::thorough::timeout_from_duration(
        duration,
        cfg.thorough_timeout_multiplier,
        cfg.thorough_timeout_floor_secs,
    )
}

/// Resolve the effective HW decode backend for `voom verify run --thorough`.
///
/// Precedence: explicit `--hw-accel` flag → `[plugin.verifier]
/// thorough_hw_accel` config → `none`. Probes `ffmpeg -hwaccels` once and
/// falls back to CPU when the requested backend is missing.
fn resolve_thorough_hw_accel(
    verifier_cfg: &VerifierConfig,
    flag: Option<HwAccelArg>,
) -> Option<HwAccelMode> {
    let raw = flag
        .map(|f| f.as_canonical().to_string())
        .unwrap_or_else(|| verifier_cfg.thorough_hw_accel.clone());
    let mode = HwAccelMode::parse(&raw).unwrap_or_else(|| {
        eprintln!(
            "warning: unrecognised hw-accel value '{raw}'; \
             valid: none, auto, nvdec, vaapi, qsv, videotoolbox. Using CPU."
        );
        HwAccelMode::None
    });
    if matches!(mode, HwAccelMode::None) {
        return None;
    }
    let probed = verifier_hwaccel::probe_hwaccels(&verifier_cfg.ffmpeg_path);
    let resolved = verifier_hwaccel::resolve(mode, &probed);
    match resolved {
        Some(m) => {
            eprintln!("Using hw-accel decode: {} (probed: {probed:?})", m.as_str());
        }
        None => {
            eprintln!(
                "Requested hw-accel '{raw}' not available on this ffmpeg \
                 (probed: {probed:?}); falling back to CPU decode."
            );
        }
    }
    resolved
}

fn read_verifier_config(cfg: &AppConfig) -> VerifierConfig {
    cfg.plugin
        .get("verifier")
        .and_then(|t| serde_json::to_value(t).ok())
        .and_then(|v| serde_json::from_value::<VerifierConfig>(v).ok())
        .unwrap_or_default()
}

fn print_record(rec: &VerificationRecord, path: &Path) {
    let status = match rec.outcome {
        VerificationOutcome::Ok => style("OK").green().to_string(),
        VerificationOutcome::Warning => style("WARN").yellow().to_string(),
        VerificationOutcome::Error => style("ERR").red().to_string(),
    };
    println!("{status:>5}  {:<8}  {}", rec.mode.as_str(), path.display());
}

fn print_summary(records: &[VerificationRecord]) {
    let ok = records
        .iter()
        .filter(|r| r.outcome == VerificationOutcome::Ok)
        .count();
    let warn = records
        .iter()
        .filter(|r| r.outcome == VerificationOutcome::Warning)
        .count();
    let err = records
        .iter()
        .filter(|r| r.outcome == VerificationOutcome::Error)
        .count();
    println!();
    println!(
        "Verified {} file(s): {ok} ok, {warn} warning, {err} error",
        records.len()
    );
}

fn run_report(args: VerifyReportArgs) -> Result<()> {
    let cfg = config::load_config().unwrap_or_default();
    let store = app::open_store(&cfg)?;

    let mut filters = VerificationFilters::default();
    filters.limit = Some(args.limit);
    if let Some(s) = args.since {
        filters.since = Some(parse_since(&s).context("parsing --since")?);
    }
    if let Some(m) = args.mode {
        filters.mode =
            Some(VerificationMode::parse(&m).ok_or_else(|| anyhow!("invalid --mode '{m}'"))?);
    }
    if let Some(o) = args.outcome {
        filters.outcome =
            Some(VerificationOutcome::parse(&o).ok_or_else(|| anyhow!("invalid --outcome '{o}'"))?);
    }
    if let Some(p) = args.file {
        let canon = std::fs::canonicalize(&p).unwrap_or(p);
        let id = store
            .file_by_path(&canon)?
            .map(|f| f.id.to_string())
            .ok_or_else(|| anyhow!("no file in DB at {}", canon.display()))?;
        filters.file_id = Some(id);
    }

    let records = store.list_verifications(&filters)?;
    match args.format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&records)?;
            println!("{json}");
        }
        OutputFormat::Plain | OutputFormat::Csv | OutputFormat::Table => {
            if records.is_empty() {
                eprintln!("No verification records.");
                return Ok(());
            }
            println!(
                "{:<24} {:<10} {:<8} {:<7} {:<7}",
                "When", "Mode", "Outcome", "Errors", "Warnings"
            );
            println!("{}", "-".repeat(76));
            for r in &records {
                println!(
                    "{:<24} {:<10} {:<8} {:<7} {:<7}",
                    r.verified_at.format("%Y-%m-%d %H:%M"),
                    r.mode.as_str(),
                    r.outcome.as_str(),
                    r.error_count,
                    r.warning_count,
                );
            }
        }
    }
    Ok(())
}
