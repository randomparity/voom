use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio_util::sync::CancellationToken;

use crate::app;
use crate::cli::{ErrorHandling, EstimateArgs, ProcessArgs};

pub async fn run(args: EstimateArgs, quiet: bool, token: CancellationToken) -> Result<()> {
    if args.is_calibration_request() {
        return calibrate(&args).await;
    }
    crate::commands::process::run(args.into_process_args(), quiet, token).await
}

impl EstimateArgs {
    fn is_calibration_request(&self) -> bool {
        self.paths.len() == 1
            && self.paths[0] == Path::new("calibrate")
            && self.policy.is_none()
            && self.policy_map.is_none()
    }

    fn into_process_args(self) -> ProcessArgs {
        ProcessArgs {
            paths: self.paths,
            policy: self.policy,
            policy_map: self.policy_map,
            dry_run: false,
            estimate: true,
            estimate_only: false,
            on_error: ErrorHandling::Fail,
            workers: self.workers,
            approve: false,
            no_backup: false,
            force_rescan: self.force_rescan,
            flag_size_increase: false,
            flag_duration_shrink: false,
            plan_only: false,
            confirm_savings: None,
            priority_by_date: false,
            execute_during_discovery: false,
        }
    }
}

async fn calibrate(args: &EstimateArgs) -> Result<()> {
    let config = crate::config::load_config()?;
    let app::BootstrapResult { store, .. } = crate::app::bootstrap_kernel_with_store(&config)?;
    let completed_at = chrono::Utc::now();
    let samples = if let Some(corpus) = &args.benchmark_corpus {
        let result = benchmark_calibration_samples(corpus, args.max_fixtures)?;
        print_accuracy_report(&result.accuracy);
        result.samples
    } else {
        default_calibration_samples(completed_at)
    };
    for sample in &samples {
        store.insert_cost_model_sample(sample)?;
    }
    println!("Recorded {} estimate calibration samples.", samples.len());
    Ok(())
}

fn default_calibration_samples(
    completed_at: chrono::DateTime<chrono::Utc>,
) -> Vec<voom_domain::CostModelSample> {
    vec![
        voom_domain::CostModelSample::new(
            voom_domain::EstimateOperationKey::transcode("video", "hevc", "slow", "software"),
            1_500_000.0,
            0.55,
            1_000,
            completed_at,
        ),
        voom_domain::CostModelSample::new(
            voom_domain::EstimateOperationKey::transcode("video", "hevc", "slow", "nvenc"),
            5_000_000.0,
            0.60,
            1_000,
            completed_at,
        ),
        voom_domain::CostModelSample::new(
            voom_domain::EstimateOperationKey::transcode("video", "av1", "slow", "software"),
            750_000.0,
            0.45,
            1_000,
            completed_at,
        ),
    ]
}

#[derive(Clone)]
struct CalibrationBenchmarkResult {
    width: u32,
    height: u32,
    duration_seconds: f64,
    input_bytes: u64,
    output_bytes: u64,
    elapsed_ms: u64,
    completed_at: chrono::DateTime<chrono::Utc>,
}

fn cost_model_sample_from_benchmark(
    result: CalibrationBenchmarkResult,
) -> voom_domain::CostModelSample {
    let pixels = f64::from(result.width) * f64::from(result.height) * result.duration_seconds;
    let elapsed_seconds = (result.elapsed_ms as f64 / 1_000.0).max(0.001);
    let output_size_ratio = result.output_bytes as f64 / result.input_bytes.max(1) as f64;

    voom_domain::CostModelSample::new(
        voom_domain::EstimateOperationKey::transcode("video", "hevc", "slow", "software"),
        pixels / elapsed_seconds,
        output_size_ratio,
        0,
        result.completed_at,
    )
}

struct CalibrationBenchmarkSamples {
    samples: Vec<voom_domain::CostModelSample>,
    accuracy: EstimateAccuracyReport,
}

struct EstimateAccuracyReport {
    fixtures: usize,
    validation_fixtures: usize,
    total_savings_error: f64,
    total_bytes_error: f64,
    total_time_error: f64,
    max_bytes_error: f64,
    max_time_error: f64,
}

fn benchmark_calibration_samples(
    corpus: &Path,
    max_fixtures: usize,
) -> Result<CalibrationBenchmarkSamples> {
    if max_fixtures == 0 {
        bail!("--max-fixtures must be greater than zero");
    }
    let fixtures = corpus_media_files(corpus, max_fixtures)?;
    if fixtures.is_empty() {
        bail!(
            "no generated media fixtures found in {}; run scripts/generate-test-corpus first",
            corpus.display()
        );
    }

    let mut results = Vec::with_capacity(fixtures.len());
    for fixture in fixtures {
        let result = run_calibration_benchmark(&fixture)
            .with_context(|| format!("failed to benchmark {}", fixture.display()))?;
        results.push(result);
    }
    let samples = results
        .iter()
        .cloned()
        .map(cost_model_sample_from_benchmark)
        .collect();
    let accuracy = accuracy_report_from_benchmarks(&results);
    Ok(CalibrationBenchmarkSamples { accuracy, samples })
}

fn accuracy_report_from_benchmarks(
    results: &[CalibrationBenchmarkResult],
) -> EstimateAccuracyReport {
    if results.len() < 2 {
        return EstimateAccuracyReport {
            fixtures: results.len(),
            validation_fixtures: 0,
            total_savings_error: 0.0,
            total_bytes_error: 0.0,
            total_time_error: 0.0,
            max_bytes_error: 0.0,
            max_time_error: 0.0,
        };
    }

    let mut estimated_bytes = 0.0_f64;
    let mut actual_bytes = 0.0_f64;
    let mut input_bytes = 0.0_f64;
    let mut estimated_ms = 0.0_f64;
    let mut actual_ms = 0.0_f64;
    let mut max_bytes_error = 0.0_f64;
    let mut max_time_error = 0.0_f64;
    for index in 0..results.len() {
        let samples = holdout_samples(results, index);
        let bytes = estimate_bytes(&results[index], &samples);
        let time = estimate_ms(&results[index], &samples);
        estimated_bytes += bytes;
        actual_bytes += results[index].output_bytes as f64;
        input_bytes += results[index].input_bytes as f64;
        estimated_ms += time;
        actual_ms += results[index].elapsed_ms as f64;
        max_bytes_error =
            max_bytes_error.max(ratio_error(bytes, results[index].output_bytes as f64));
        max_time_error = max_time_error.max(ratio_error(time, results[index].elapsed_ms as f64));
    }

    EstimateAccuracyReport {
        fixtures: results.len(),
        validation_fixtures: results.len(),
        total_savings_error: ratio_error(input_bytes - estimated_bytes, input_bytes - actual_bytes),
        total_bytes_error: ratio_error(estimated_bytes, actual_bytes),
        total_time_error: ratio_error(estimated_ms, actual_ms),
        max_bytes_error,
        max_time_error,
    }
}

fn holdout_samples(
    results: &[CalibrationBenchmarkResult],
    holdout_index: usize,
) -> Vec<voom_domain::CostModelSample> {
    results
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != holdout_index)
        .map(|(_, result)| cost_model_sample_from_benchmark(result.clone()))
        .collect()
}

fn estimate_bytes(
    result: &CalibrationBenchmarkResult,
    samples: &[voom_domain::CostModelSample],
) -> f64 {
    let output_size_ratio = median_f64(samples.iter().map(|sample| sample.output_size_ratio));
    (result.input_bytes as f64 * output_size_ratio).round()
}

fn estimate_ms(
    result: &CalibrationBenchmarkResult,
    samples: &[voom_domain::CostModelSample],
) -> f64 {
    let pixels_per_second = median_f64(samples.iter().map(|sample| sample.pixels_per_second));
    let pixels = f64::from(result.width) * f64::from(result.height) * result.duration_seconds;
    pixels / pixels_per_second * 1_000.0
}

fn ratio_error(estimated: f64, actual: f64) -> f64 {
    if actual <= 0.0 {
        return 0.0;
    }
    (estimated - actual).abs() / actual
}

fn print_accuracy_report(report: &EstimateAccuracyReport) {
    if report.validation_fixtures == 0 {
        println!(
            "Benchmark estimate accuracy: insufficient holdout fixtures \
             ({} fixture); run with --max-fixtures 2 or more",
            report.fixtures
        );
        return;
    }
    println!(
        "Benchmark estimate accuracy: {} fixtures, total savings error {:.1}%, \
         total bytes error {:.1}%, total time error {:.1}%, max file bytes error {:.1}%, \
         max file time error {:.1}%",
        report.validation_fixtures,
        report.total_savings_error * 100.0,
        report.total_bytes_error * 100.0,
        report.total_time_error * 100.0,
        report.max_bytes_error * 100.0,
        report.max_time_error * 100.0
    );
}

fn median_f64(values: impl Iterator<Item = f64>) -> f64 {
    let mut values: Vec<f64> = values.collect();
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn corpus_media_files(corpus: &Path, max_fixtures: usize) -> Result<Vec<PathBuf>> {
    if !corpus.is_dir() {
        bail!("benchmark corpus is not a directory: {}", corpus.display());
    }

    let manifest = corpus.join("manifest.json");
    if manifest.exists() {
        return corpus_media_files_from_manifest(corpus, &manifest, max_fixtures);
    }

    let mut files = Vec::new();
    for entry in std::fs::read_dir(corpus)
        .with_context(|| format!("failed to read corpus directory {}", corpus.display()))?
    {
        let path = entry?.path();
        if is_media_file(&path) {
            files.push(path);
        }
    }
    files.sort();
    files.truncate(max_fixtures);
    Ok(files)
}

fn corpus_media_files_from_manifest(
    corpus: &Path,
    manifest: &Path,
    max_fixtures: usize,
) -> Result<Vec<PathBuf>> {
    let text = std::fs::read_to_string(manifest)
        .with_context(|| format!("failed to read {}", manifest.display()))?;
    let manifest: CorpusManifest = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", manifest.display()))?;
    let mut files = Vec::new();
    for entry in manifest.generated.into_iter().take(max_fixtures) {
        let path = corpus.join(entry.filename);
        if is_media_file(&path) {
            files.push(path);
        }
    }
    Ok(files)
}

fn is_media_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|value| value.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "avi" | "flv" | "mkv" | "mov" | "mp4" | "ts" | "webm" | "wmv"
    )
}

fn run_calibration_benchmark(path: &Path) -> Result<CalibrationBenchmarkResult> {
    let probe = probe_video(path)?;
    let benchmark_duration = benchmark_duration_seconds(probe.duration_seconds);
    let input_bytes = path
        .metadata()
        .with_context(|| format!("failed to stat {}", path.display()))?
        .len();
    let output = std::env::temp_dir().join(format!("voom-calibrate-{}.mkv", uuid::Uuid::new_v4()));
    let args = vec![
        "-y".to_string(),
        "-v".to_string(),
        "error".to_string(),
        "-i".to_string(),
        path.display().to_string(),
        "-map".to_string(),
        "0:v:0".to_string(),
        "-t".to_string(),
        benchmark_duration.to_string(),
        "-an".to_string(),
        "-c:v".to_string(),
        "libx265".to_string(),
        "-preset".to_string(),
        "slow".to_string(),
        output.display().to_string(),
    ];

    let start = Instant::now();
    let command_output = voom_process::run_with_timeout("ffmpeg", &args, Duration::from_secs(300))?;
    let elapsed_ms = start.elapsed().as_millis() as u64;
    if !command_output.status.success() {
        let stderr = String::from_utf8_lossy(&command_output.stderr);
        bail!("ffmpeg calibration transcode failed: {}", stderr.trim());
    }

    let output_bytes = output
        .metadata()
        .with_context(|| format!("failed to stat calibration output {}", output.display()))?
        .len();
    std::fs::remove_file(&output)
        .with_context(|| format!("failed to remove calibration output {}", output.display()))?;

    Ok(CalibrationBenchmarkResult {
        width: probe.width,
        height: probe.height,
        duration_seconds: benchmark_duration,
        input_bytes,
        output_bytes,
        elapsed_ms,
        completed_at: chrono::Utc::now(),
    })
}

fn benchmark_duration_seconds(duration_seconds: f64) -> f64 {
    if !duration_seconds.is_finite() || duration_seconds <= 0.0 {
        return 0.1;
    }
    duration_seconds.clamp(0.1, 2.0)
}

fn probe_video(path: &Path) -> Result<VideoProbe> {
    let args = vec![
        "-v".to_string(),
        "error".to_string(),
        "-select_streams".to_string(),
        "v:0".to_string(),
        "-show_entries".to_string(),
        "stream=width,height:format=duration".to_string(),
        "-of".to_string(),
        "json".to_string(),
        path.display().to_string(),
    ];
    let output = voom_process::run_with_timeout("ffprobe", &args, Duration::from_secs(30))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ffprobe failed for {}: {}", path.display(), stderr.trim());
    }
    let probe: FfprobeOutput = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("failed to parse ffprobe JSON for {}", path.display()))?;
    let stream = probe
        .streams
        .first()
        .context("ffprobe did not return a video stream")?;
    let duration_seconds = probe
        .format
        .duration
        .as_deref()
        .unwrap_or("0")
        .parse::<f64>()
        .context("ffprobe returned invalid duration")?;

    Ok(VideoProbe {
        width: stream.width,
        height: stream.height,
        duration_seconds,
    })
}

#[derive(serde::Deserialize)]
struct CorpusManifest {
    generated: Vec<CorpusManifestEntry>,
}

#[derive(serde::Deserialize)]
struct CorpusManifestEntry {
    filename: String,
}

struct VideoProbe {
    width: u32,
    height: u32,
    duration_seconds: f64,
}

#[derive(serde::Deserialize)]
struct FfprobeOutput {
    streams: Vec<FfprobeStream>,
    format: FfprobeFormat,
}

#[derive(serde::Deserialize)]
struct FfprobeStream {
    width: u32,
    height: u32,
}

#[derive(serde::Deserialize)]
struct FfprobeFormat {
    duration: Option<String>,
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use crate::commands::estimate::{
        CalibrationBenchmarkResult, accuracy_report_from_benchmarks,
        cost_model_sample_from_benchmark,
    };

    #[test]
    fn calibration_benchmark_records_measured_speed_and_ratio() {
        let completed_at = chrono::Utc
            .with_ymd_and_hms(2026, 5, 10, 12, 0, 0)
            .single()
            .expect("valid timestamp");
        let result = CalibrationBenchmarkResult {
            width: 1920,
            height: 1080,
            duration_seconds: 2.0,
            input_bytes: 1_000_000,
            output_bytes: 420_000,
            elapsed_ms: 1_036,
            completed_at,
        };

        let sample = cost_model_sample_from_benchmark(result);

        assert_eq!(sample.key.phase_name, "video");
        assert_eq!(sample.key.codec, "hevc");
        assert_eq!(sample.key.preset, "slow");
        assert_eq!(sample.key.backend, "software");
        assert_eq!(sample.output_size_ratio, 0.42);
        assert_eq!(sample.fixed_overhead_ms, 0);
        assert_eq!(sample.pixels_per_second.round() as u64, 4_003_089);
    }

    #[test]
    fn calibration_accuracy_reports_insufficient_single_fixture_holdout() {
        let result = CalibrationBenchmarkResult {
            width: 1280,
            height: 720,
            duration_seconds: 1.0,
            input_bytes: 1_000_000,
            output_bytes: 500_000,
            elapsed_ms: 500,
            completed_at: chrono::Utc::now(),
        };

        let report = accuracy_report_from_benchmarks(&[result]);

        assert_eq!(report.fixtures, 1);
        assert_eq!(report.validation_fixtures, 0);
        assert_eq!(report.total_savings_error, 0.0);
        assert_eq!(report.total_bytes_error, 0.0);
        assert_eq!(report.total_time_error, 0.0);
        assert_eq!(report.max_bytes_error, 0.0);
        assert_eq!(report.max_time_error, 0.0);
    }

    #[test]
    fn calibration_accuracy_uses_holdout_fixtures() {
        let first = CalibrationBenchmarkResult {
            width: 1280,
            height: 720,
            duration_seconds: 1.0,
            input_bytes: 1_000_000,
            output_bytes: 500_000,
            elapsed_ms: 500,
            completed_at: chrono::Utc::now(),
        };
        let second = CalibrationBenchmarkResult {
            width: 1280,
            height: 720,
            duration_seconds: 1.0,
            input_bytes: 1_000_000,
            output_bytes: 250_000,
            elapsed_ms: 1_000,
            completed_at: chrono::Utc::now(),
        };

        let report = accuracy_report_from_benchmarks(&[first, second]);

        assert_eq!(report.fixtures, 2);
        assert!(report.max_bytes_error > 0.0);
        assert!(report.max_time_error > 0.0);
    }

    #[test]
    fn calibration_accuracy_reports_total_holdout_error() {
        let completed_at = chrono::Utc::now();
        let first = CalibrationBenchmarkResult {
            width: 1280,
            height: 720,
            duration_seconds: 1.0,
            input_bytes: 1_000_000,
            output_bytes: 500_000,
            elapsed_ms: 500,
            completed_at,
        };
        let second = CalibrationBenchmarkResult {
            width: 1280,
            height: 720,
            duration_seconds: 1.0,
            input_bytes: 1_000_000,
            output_bytes: 250_000,
            elapsed_ms: 1_000,
            completed_at,
        };
        let third = CalibrationBenchmarkResult {
            width: 1280,
            height: 720,
            duration_seconds: 1.0,
            input_bytes: 1_000_000,
            output_bytes: 250_000,
            elapsed_ms: 1_000,
            completed_at,
        };

        let report = accuracy_report_from_benchmarks(&[first, second, third]);

        assert_eq!(report.validation_fixtures, 3);
        assert_eq!(report.total_savings_error, 0.125);
        assert_eq!(report.total_bytes_error, 0.25);
        assert!(report.total_time_error > 0.0);
    }

    #[test]
    fn benchmark_duration_stays_bounded_for_invalid_probe_values() {
        assert_eq!(
            crate::commands::estimate::benchmark_duration_seconds(5.0),
            2.0
        );
        assert_eq!(
            crate::commands::estimate::benchmark_duration_seconds(0.05),
            0.1
        );
        assert_eq!(
            crate::commands::estimate::benchmark_duration_seconds(f64::NAN),
            0.1
        );
    }
}
