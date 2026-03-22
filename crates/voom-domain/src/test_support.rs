//! Shared in-memory `StorageTrait` implementation for testing.
//!
//! Gated behind the `testing` feature. Enable in your crate's
//! `[dev-dependencies]` with:
//!
//! ```toml
//! voom-domain = { path = "...", features = ["testing"] }
//! ```

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use uuid::Uuid;

use crate::bad_file::BadFile;
use crate::errors::{Result, VoomError};
use crate::job::{Job, JobStatus, JobUpdate};
use crate::media::MediaFile;
use crate::plan::Plan;
use crate::stats::ProcessingStats;
use crate::storage::{
    BadFileFilters, BadFileStorage, FileFilters, FileHistoryStorage, FileStorage, JobFilters,
    JobStorage, MaintenanceStorage, PlanStorage, PluginDataStorage, StatsStorage, StoredPlan,
};

/// In-memory storage for testing. Implements the full `StorageTrait` via
/// sub-traits with working file and job methods. Plan/stats/plugin-data
/// methods are stubs.
pub struct InMemoryStore {
    files: Mutex<HashMap<Uuid, MediaFile>>,
    jobs: Mutex<HashMap<Uuid, Job>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            files: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
        }
    }

    /// Builder: seed the store with a file.
    pub fn with_file(self, file: MediaFile) -> Self {
        self.files.lock().unwrap().insert(file.id, file);
        self
    }

    /// Builder: seed the store with a job.
    pub fn with_job(self, job: Job) -> Self {
        self.jobs.lock().unwrap().insert(job.id, job);
        self
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl FileStorage for InMemoryStore {
    fn upsert_file(&self, file: &MediaFile) -> Result<()> {
        self.files.lock().unwrap().insert(file.id, file.clone());
        Ok(())
    }

    fn get_file(&self, id: &Uuid) -> Result<Option<MediaFile>> {
        Ok(self.files.lock().unwrap().get(id).cloned())
    }

    fn get_file_by_path(&self, path: &Path) -> Result<Option<MediaFile>> {
        Ok(self
            .files
            .lock()
            .unwrap()
            .values()
            .find(|f| f.path == path)
            .cloned())
    }

    fn list_files(&self, filters: &FileFilters) -> Result<Vec<MediaFile>> {
        let files = self.files.lock().unwrap();
        let mut result: Vec<MediaFile> = files
            .values()
            .filter(|f| {
                if let Some(container) = filters.container {
                    if f.container != container {
                        return false;
                    }
                }
                if let Some(ref prefix) = filters.path_prefix {
                    if !f.path.to_string_lossy().starts_with(prefix.as_str()) {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();
        result.sort_by(|a, b| a.path.cmp(&b.path));
        if let Some(offset) = filters.offset {
            result = result.into_iter().skip(offset as usize).collect();
        }
        if let Some(limit) = filters.limit {
            result.truncate(limit as usize);
        }
        Ok(result)
    }

    fn count_files(&self, filters: &FileFilters) -> Result<u64> {
        let files = self.files.lock().unwrap();
        let count = files
            .values()
            .filter(|f| {
                if let Some(container) = filters.container {
                    if f.container != container {
                        return false;
                    }
                }
                if let Some(ref prefix) = filters.path_prefix {
                    if !f.path.to_string_lossy().starts_with(prefix.as_str()) {
                        return false;
                    }
                }
                true
            })
            .count();
        Ok(count as u64)
    }

    fn delete_file(&self, id: &Uuid) -> Result<()> {
        self.files.lock().unwrap().remove(id);
        Ok(())
    }
}

impl JobStorage for InMemoryStore {
    fn create_job(&self, job: &Job) -> Result<Uuid> {
        self.jobs.lock().unwrap().insert(job.id, job.clone());
        Ok(job.id)
    }

    fn get_job(&self, id: &Uuid) -> Result<Option<Job>> {
        Ok(self.jobs.lock().unwrap().get(id).cloned())
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
            job.progress_message.clone_from(msg);
        }
        if let Some(ref output) = update.output {
            job.output.clone_from(output);
        }
        if let Some(ref error) = update.error {
            job.error.clone_from(error);
        }
        if let Some(ref worker) = update.worker_id {
            job.worker_id.clone_from(worker);
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

    fn claim_job_by_id(&self, job_id: &Uuid, worker_id: &str) -> Result<Option<Job>> {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(job) = jobs.get_mut(job_id) {
            if job.status == JobStatus::Pending {
                job.status = JobStatus::Running;
                job.worker_id = Some(worker_id.to_string());
                job.started_at = Some(chrono::Utc::now());
                return Ok(Some(job.clone()));
            }
        }
        Ok(None)
    }

    fn list_jobs(&self, filters: &JobFilters) -> Result<Vec<Job>> {
        let jobs = self.jobs.lock().unwrap();
        let mut result: Vec<Job> = jobs
            .values()
            .filter(|j| filters.status.is_none_or(|s| j.status == s))
            .cloned()
            .collect();
        result.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then(b.created_at.cmp(&a.created_at))
        });
        if let Some(limit) = filters.limit {
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
}

impl PlanStorage for InMemoryStore {
    fn save_plan(&self, _plan: &Plan) -> Result<Uuid> {
        Ok(Uuid::new_v4())
    }

    fn get_plans_for_file(&self, _file_id: &Uuid) -> Result<Vec<StoredPlan>> {
        Ok(Vec::new())
    }

    fn update_plan_status(
        &self,
        _plan_id: &Uuid,
        _status: crate::storage::PlanStatus,
    ) -> Result<()> {
        Ok(())
    }
}

impl FileHistoryStorage for InMemoryStore {
    fn get_file_history(&self, _path: &Path) -> Result<Vec<crate::storage::FileHistoryEntry>> {
        Ok(vec![])
    }
}

impl StatsStorage for InMemoryStore {
    fn record_stats(&self, _stats: &ProcessingStats) -> Result<()> {
        Ok(())
    }
}

impl PluginDataStorage for InMemoryStore {
    fn get_plugin_data(&self, _plugin: &str, _key: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn set_plugin_data(&self, _plugin: &str, _key: &str, _value: &[u8]) -> Result<()> {
        Ok(())
    }

    fn delete_plugin_data(&self, _plugin: &str, _key: &str) -> Result<()> {
        Ok(())
    }
}

impl BadFileStorage for InMemoryStore {
    fn upsert_bad_file(&self, _bad_file: &BadFile) -> Result<()> {
        Ok(())
    }

    fn get_bad_file_by_path(&self, _path: &Path) -> Result<Option<BadFile>> {
        Ok(None)
    }

    fn list_bad_files(&self, _filters: &BadFileFilters) -> Result<Vec<BadFile>> {
        Ok(Vec::new())
    }

    fn count_bad_files(&self) -> Result<u64> {
        Ok(0)
    }

    fn delete_bad_file(&self, _id: &Uuid) -> Result<()> {
        Ok(())
    }

    fn delete_bad_file_by_path(&self, _path: &Path) -> Result<()> {
        Ok(())
    }
}

impl MaintenanceStorage for InMemoryStore {
    fn vacuum(&self) -> Result<()> {
        Ok(())
    }

    fn prune_missing_files(&self) -> Result<u64> {
        Ok(0)
    }

    fn prune_missing_files_under(&self, _root: &Path) -> Result<u64> {
        Ok(0)
    }
}
