use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::cli::{ErrorHandling, EstimateArgs, ProcessArgs};

pub async fn run(args: EstimateArgs, quiet: bool, token: CancellationToken) -> Result<()> {
    crate::commands::process::run(args.into_process_args(), quiet, token).await
}

impl EstimateArgs {
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
