//! Plan-level resource limiter for executor-bound work.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Semaphore;

/// Canonical plan-level hardware resource classification.
///
/// Classification is intentionally plan-level: the first matching hardware
/// video transcode action determines the resource for the whole plan. Audio
/// transcodes and unknown hardware values do not consume video encoder permits.
struct PlanParallelResource;

impl PlanParallelResource {
    /// Return the plan-level hardware resource, using `default_resource` for
    /// video transcodes with `hw: auto` or no per-action hardware setting.
    ///
    /// This mirrors ffmpeg-executor's global hardware config: explicit backend
    /// names win, `hw: none` stays software, and unspecified/`auto` plans use
    /// the active executor-level hardware backend when one has a limit.
    #[must_use]
    pub fn from_plan_with_default<'a>(
        plan: &voom_domain::plan::Plan,
        default_resource: Option<&'a str>,
    ) -> Option<&'a str> {
        for action in &plan.actions {
            if action.operation != voom_domain::plan::OperationType::TranscodeVideo {
                continue;
            }
            let voom_domain::plan::ActionParams::Transcode { settings, .. } = &action.parameters
            else {
                continue;
            };
            match settings.hw.as_deref() {
                Some("nvenc") => return Some("hw:nvenc"),
                Some("qsv") => return Some("hw:qsv"),
                Some("vaapi") => return Some("hw:vaapi"),
                Some("videotoolbox") => return Some("hw:videotoolbox"),
                Some("auto") | None => {
                    if let Some(resource) = default_resource {
                        return Some(resource);
                    }
                }
                _ => {}
            }
        }
        None
    }
}

/// Plan-level concurrency limiter for resources announced by executor capabilities.
///
/// This bounds concurrent plan executions that use a classified hardware video
/// encoder resource. It does not count individual tracks or exact encoder
/// sessions within a plan. Callers that need executor-level hardware limits to
/// apply to plans with `hw: auto` or no per-action hardware should use
/// [`PlanExecutionLimiter::from_limits_with_default`].
type PlanAcquireObserver = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone, Default)]
pub struct PlanExecutionLimiter {
    semaphores: Arc<HashMap<String, Arc<Semaphore>>>,
    default_resource: Option<String>,
    acquire_observer: Option<PlanAcquireObserver>,
}

/// RAII permit for a classified plan resource.
///
/// Dropping the permit releases the resource. Permits may be no-ops when the
/// plan has no classified resource or no configured limit.
pub struct PlanExecutionPermit {
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl PlanExecutionLimiter {
    /// Create a limiter from resource limits.
    ///
    /// Positive limits create semaphores. Zero limits are ignored, leaving that
    /// resource unlimited. Plans with `hw: auto` or missing per-action hardware
    /// are not limited unless callers use
    /// [`PlanExecutionLimiter::from_limits_with_default`].
    #[must_use]
    pub fn from_limits(limits: impl IntoIterator<Item = (String, usize)>) -> Self {
        Self::from_limits_with_default(limits, None)
    }

    /// Create a limiter from resource limits and an explicit default resource.
    ///
    /// Positive limits create semaphores. Zero limits are ignored, leaving that
    /// resource unlimited. `default_resource` is used for `hw: auto` or missing
    /// per-action hardware only when it matches a positive configured limit.
    #[must_use]
    pub fn from_limits_with_default(
        limits: impl IntoIterator<Item = (String, usize)>,
        default_resource: Option<String>,
    ) -> Self {
        let mut semaphores = HashMap::new();
        for (resource, limit) in limits {
            if limit == 0 {
                continue;
            }
            semaphores.insert(resource, Arc::new(Semaphore::new(limit)));
        }
        let default_resource =
            default_resource.filter(|resource| semaphores.contains_key(resource));
        Self {
            semaphores: Arc::new(semaphores),
            default_resource,
            acquire_observer: None,
        }
    }

    /// Attach a callback invoked after a plan maps to a limited resource and
    /// before the limiter waits for capacity.
    #[must_use]
    pub fn with_acquire_observer(
        mut self,
        observer: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        self.acquire_observer = Some(Arc::new(observer));
        self
    }

    /// Acquire a plan-level resource permit for a plan.
    ///
    /// This is a no-op for plans without a classified hardware video transcode
    /// resource or with no configured limit for that resource. Otherwise, this
    /// waits for capacity and holds it until the returned permit is dropped.
    pub async fn acquire_for_plan(&self, plan: &voom_domain::plan::Plan) -> PlanExecutionPermit {
        let Some(resource) =
            PlanParallelResource::from_plan_with_default(plan, self.default_resource.as_deref())
        else {
            return PlanExecutionPermit { _permit: None };
        };
        let Some(semaphore) = self.semaphores.get(resource) else {
            return PlanExecutionPermit { _permit: None };
        };
        if let Some(observer) = &self.acquire_observer {
            observer(resource);
        }
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("plan execution semaphore closed");
        PlanExecutionPermit {
            _permit: Some(permit),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_transcode_plan(hw: Option<&str>) -> voom_domain::plan::Plan {
        test_transcode_plan_with_operation(hw, voom_domain::plan::OperationType::TranscodeVideo)
    }

    fn test_transcode_plan_with_operation(
        hw: Option<&str>,
        operation: voom_domain::plan::OperationType,
    ) -> voom_domain::plan::Plan {
        let file = voom_domain::media::MediaFile::new(std::path::PathBuf::from("/test.mkv"));
        voom_domain::plan::Plan::new(file, "test-policy", "test-phase").with_action(
            voom_domain::plan::PlannedAction::track_op(
                operation,
                0,
                voom_domain::plan::ActionParams::Transcode {
                    codec: "hevc".to_string(),
                    settings: voom_domain::plan::TranscodeSettings::default()
                        .with_hw(hw.map(str::to_string)),
                },
                "transcode video",
            ),
        )
    }

    #[test]
    fn test_plan_parallel_resource_detects_nvenc_transcode() {
        let plan = test_transcode_plan(Some("nvenc"));
        assert_eq!(
            PlanParallelResource::from_plan_with_default(&plan, None),
            Some("hw:nvenc")
        );
    }

    #[test]
    fn test_plan_parallel_resource_ignores_software_transcode() {
        let plan = test_transcode_plan(None);
        assert_eq!(
            PlanParallelResource::from_plan_with_default(&plan, None),
            None
        );
    }

    #[test]
    fn test_plan_parallel_resource_classifies_known_video_hw() {
        let cases = [
            (Some("nvenc"), Some("hw:nvenc")),
            (Some("qsv"), Some("hw:qsv")),
            (Some("vaapi"), Some("hw:vaapi")),
            (Some("videotoolbox"), Some("hw:videotoolbox")),
            (Some("none"), None),
            (Some("auto"), None),
            (Some("mysteryhw"), None),
            (None, None),
        ];

        for (hw, expected) in cases {
            let plan = test_transcode_plan(hw);
            assert_eq!(
                PlanParallelResource::from_plan_with_default(&plan, None),
                expected,
                "hw={hw:?}"
            );
        }
    }

    #[test]
    fn test_plan_parallel_resource_uses_default_for_auto_and_missing_hw() {
        for hw in [Some("auto"), None] {
            let plan = test_transcode_plan(hw);
            assert_eq!(
                PlanParallelResource::from_plan_with_default(&plan, Some("hw:nvenc")),
                Some("hw:nvenc"),
                "hw={hw:?}"
            );
        }
    }

    #[test]
    fn test_plan_parallel_resource_default_does_not_override_none() {
        let plan = test_transcode_plan(Some("none"));
        assert_eq!(
            PlanParallelResource::from_plan_with_default(&plan, Some("hw:nvenc")),
            None
        );
    }

    #[test]
    fn test_plan_parallel_resource_ignores_audio_transcode_hw() {
        let plan = test_transcode_plan_with_operation(
            Some("nvenc"),
            voom_domain::plan::OperationType::TranscodeAudio,
        );
        assert_eq!(
            PlanParallelResource::from_plan_with_default(&plan, None),
            None
        );
    }

    #[tokio::test]
    async fn test_plan_execution_limiter_blocks_missing_hw_on_default_resource() {
        use std::time::Duration;

        let limiter = PlanExecutionLimiter::from_limits_with_default(
            vec![("hw:nvenc".to_string(), 1)],
            Some("hw:nvenc".to_string()),
        );
        let plan = test_transcode_plan(None);
        let first = limiter.acquire_for_plan(&plan).await;

        let limiter_clone = limiter.clone();
        let plan_clone = plan.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let third = tokio::spawn(async move {
            let _permit = limiter_clone.acquire_for_plan(&plan_clone).await;
            let _ = entered_tx.send(());
            "entered"
        });

        assert!(
            tokio::time::timeout(Duration::from_millis(20), entered_rx)
                .await
                .is_err(),
            "missing per-action hw should wait on the default limited resource"
        );

        drop(first);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), third)
                .await
                .expect("task should enter after permit release")
                .unwrap(),
            "entered"
        );
    }

    #[tokio::test]
    async fn test_plan_execution_limiter_does_not_default_without_explicit_resource() {
        use std::time::Duration;

        let limiter = PlanExecutionLimiter::from_limits(vec![("hw:nvenc".to_string(), 1)]);
        let limited_plan = test_transcode_plan(Some("nvenc"));
        let implicit_plan = test_transcode_plan(None);
        let first = limiter.acquire_for_plan(&limited_plan).await;

        let limiter_clone = limiter.clone();
        let plan_clone = implicit_plan.clone();
        let task = tokio::spawn(async move {
            let _permit = limiter_clone.acquire_for_plan(&plan_clone).await;
            "entered"
        });

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .expect("implicit plan should not wait without explicit default")
                .unwrap(),
            "entered"
        );
        drop(first);
    }

    #[tokio::test]
    async fn test_plan_execution_limiter_blocks_above_limit() {
        use std::time::Duration;

        let limiter = PlanExecutionLimiter::from_limits(vec![("hw:nvenc".to_string(), 2)]);
        let plan = test_transcode_plan(Some("nvenc"));

        let first = limiter.acquire_for_plan(&plan).await;
        let second = limiter.acquire_for_plan(&plan).await;

        let limiter_clone = limiter.clone();
        let plan_clone = plan.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let third = tokio::spawn(async move {
            let _permit = limiter_clone.acquire_for_plan(&plan_clone).await;
            let _ = entered_tx.send("entered");
            "entered"
        });

        assert!(tokio::time::timeout(Duration::from_millis(20), entered_rx)
            .await
            .is_err());

        drop(first);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), third)
                .await
                .unwrap()
                .unwrap(),
            "entered"
        );
        drop(second);
    }
}
