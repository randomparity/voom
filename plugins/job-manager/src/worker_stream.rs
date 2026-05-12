//! Streaming entry point for `WorkerPool`: consume `WorkItem`s from an
//! `mpsc::Receiver`, enqueue them into the SQLite-backed `JobQueue`, and
//! claim/process them concurrently while an `execution_gate` is held open.

use std::sync::Arc;

use tokio::sync::{Notify, mpsc};

use crate::progress::ProgressReporter;
use crate::worker::{JobErrorStrategy, JobResult, WorkItem, WorkerPool};

impl WorkerPool {
    /// Stream entries through the pool: consume items from `items`, enqueue
    /// each one into the job queue, and run up to `effective_workers` claim
    /// loops in parallel. Workers wait on `execution_gate` before their first
    /// claim. The pool returns when:
    ///
    /// 1. `producer_done` has been notified,
    /// 2. the receiver has been drained,
    /// 3. and every worker has finished its last in-flight job.
    ///
    /// Cancellation: the pool's internal token (passed via `WorkerPool::new`)
    /// is the shared signal. When cancelled, the enqueuer stops draining the
    /// receiver, the remaining queued-but-unstarted jobs are cancelled via
    /// the existing `cancel_unstarted_jobs` helper, and workers exit after
    /// their current job.
    pub async fn process_stream<P, F, Fut>(
        &self,
        items: mpsc::Receiver<WorkItem<P>>,
        producer_done: Arc<Notify>,
        execution_gate: Arc<Notify>,
        processor: F,
        on_error: JobErrorStrategy,
        reporter: Arc<dyn ProgressReporter>,
    ) -> Vec<JobResult>
    where
        P: serde::Serialize + Send + 'static,
        F: Fn(voom_domain::job::Job) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = std::result::Result<Option<serde_json::Value>, String>>
            + Send
            + 'static,
    {
        // Replaced in the next task.
        let _ = (
            items,
            producer_done,
            execution_gate,
            processor,
            on_error,
            reporter,
        );
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    // Tests follow in the next task.
}
