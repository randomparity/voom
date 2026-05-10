use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::app;
use crate::cli::{ErrorHandling, EstimateArgs, ProcessArgs};

pub async fn run(args: EstimateArgs, quiet: bool, token: CancellationToken) -> Result<()> {
    if args.is_calibration_request() {
        return calibrate().await;
    }
    crate::commands::process::run(args.into_process_args(), quiet, token).await
}

impl EstimateArgs {
    fn is_calibration_request(&self) -> bool {
        self.paths.len() == 1
            && self.paths[0] == std::path::Path::new("calibrate")
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
        }
    }
}

async fn calibrate() -> Result<()> {
    let config = crate::config::load_config()?;
    let app::BootstrapResult { store, .. } = crate::app::bootstrap_kernel_with_store(&config)?;
    let completed_at = chrono::Utc::now();
    let samples = default_calibration_samples(completed_at);
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
