//! In-memory StorageTrait implementation for testing the job manager.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use uuid::Uuid;

use voom_domain::errors::{Result, VoomError};
use voom_domain::job::{Job, JobStatus, JobUpdate};
use voom_domain::media::MediaFile;
use voom_domain::plan::Plan;
use voom_domain::stats::ProcessingStats;
use voom_domain::storage::{FileFilters, StorageTrait, StoredPlan};

/// Simple in-memory storage for testing. Only implements job methods fully.
pub struct InMemoryStore {
    jobs: Mutex<HashMap<Uuid, Job>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
        }
    }
}

impl StorageTrait for InMemoryStore {
    fn upsert_file(&self, _file: &MediaFile) -> Result<()> {
        Ok(())
    }
    fn get_file(&self, _id: &Uuid) -> Result<Option<MediaFile>> {
        Ok(None)
    }
    fn get_file_by_path(&self, _path: &Path) -> Result<Option<MediaFile>> {
        Ok(None)
    }
    fn list_files(&self, _filters: &FileFilters) -> Result<Vec<MediaFile>> {
        Ok(Vec::new())
    }
    fn delete_file(&self, _id: &Uuid) -> Result<()> {
        Ok(())
    }

    fn create_job(&self, job: &Job) -> Result<Uuid> {
        let mut jobs = self.jobs.lock().unwrap();
        jobs.insert(job.id, job.clone());
        Ok(job.id)
    }

    fn get_job(&self, id: &Uuid) -> Result<Option<Job>> {
        let jobs = self.jobs.lock().unwrap();
        Ok(jobs.get(id).cloned())
    }

    fn update_job(&self, id: &Uuid, update: &JobUpdate) -> Result<()> {
        let mut jobs = self.jobs.lock().unwrap();
        let job = jobs
            .get_mut(id)
            .ok_or_else(|| VoomError::Storage(format!("job {id} not found")))?;

        if let Some(status) = update.status {
            job.status = status;
        }
        if let Some(progress) = update.progress {
            job.progress = progress;
        }
        if let Some(ref msg) = update.progress_message {
            job.progress_message = msg.clone();
        }
        if let Some(ref output) = update.output {
            job.output = output.clone();
        }
        if let Some(ref error) = update.error {
            job.error = error.clone();
        }
        if let Some(ref worker) = update.worker_id {
            job.worker_id = worker.clone();
        }
        if let Some(ref started) = update.started_at {
            job.started_at = *started;
        }
        if let Some(ref completed) = update.completed_at {
            job.completed_at = *completed;
        }

        Ok(())
    }

    fn claim_next_job(&self, worker_id: &str) -> Result<Option<Job>> {
        let mut jobs = self.jobs.lock().unwrap();

        // Find the pending job with highest priority (lowest number)
        let job_id = jobs
            .values()
            .filter(|j| j.status == JobStatus::Pending)
            .min_by_key(|j| (j.priority, j.created_at))
            .map(|j| j.id);

        if let Some(id) = job_id {
            let job = jobs.get_mut(&id).unwrap();
            job.status = JobStatus::Running;
            job.worker_id = Some(worker_id.to_string());
            job.started_at = Some(chrono::Utc::now());
            Ok(Some(job.clone()))
        } else {
            Ok(None)
        }
    }

    fn list_jobs(&self, status: Option<JobStatus>, limit: Option<u32>) -> Result<Vec<Job>> {
        let jobs = self.jobs.lock().unwrap();
        let mut result: Vec<Job> = jobs
            .values()
            .filter(|j| status.map_or(true, |s| j.status == s))
            .cloned()
            .collect();
        result.sort_by(|a, b| a.priority.cmp(&b.priority).then(b.created_at.cmp(&a.created_at)));
        if let Some(limit) = limit {
            result.truncate(limit as usize);
        }
        Ok(result)
    }

    fn count_jobs_by_status(&self) -> Result<Vec<(JobStatus, u64)>> {
        let jobs = self.jobs.lock().unwrap();
        let mut counts: HashMap<JobStatus, u64> = HashMap::new();
        for job in jobs.values() {
            *counts.entry(job.status).or_insert(0) += 1;
        }
        Ok(counts.into_iter().collect())
    }

    fn save_plan(&self, _plan: &Plan) -> Result<Uuid> {
        Ok(Uuid::new_v4())
    }
    fn get_plans_for_file(&self, _file_id: &Uuid) -> Result<Vec<StoredPlan>> {
        Ok(Vec::new())
    }
    fn record_stats(&self, _stats: &ProcessingStats) -> Result<()> {
        Ok(())
    }
    fn get_plugin_data(&self, _plugin: &str, _key: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn set_plugin_data(&self, _plugin: &str, _key: &str, _value: &[u8]) -> Result<()> {
        Ok(())
    }
    fn vacuum(&self) -> Result<()> {
        Ok(())
    }
    fn prune_missing_files(&self) -> Result<u64> {
        Ok(0)
    }
}
