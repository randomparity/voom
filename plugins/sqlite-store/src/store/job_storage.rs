use chrono::Utc;
use rusqlite::params;
use uuid::Uuid;

use voom_domain::errors::{Result, StorageErrorKind, VoomError};
use voom_domain::job::{Job, JobStatus, JobUpdate};
use voom_domain::storage::{JobFilters, JobStorage, PruneReport, RetentionPolicy};

use super::{
    format_datetime, other_storage_err, parse_datetime, row_to_job, storage_err, SqlQuery,
    SqliteStore,
};

fn serialize_json(value: &serde_json::Value) -> Result<String> {
    serde_json::to_string(value).map_err(other_storage_err("failed to serialize JSON"))
}

impl JobStorage for SqliteStore {
    fn create_job(&self, job: &Job) -> Result<Uuid> {
        let conn = self.conn()?;
        let payload_json = job.payload.as_ref().map(serialize_json).transpose()?;
        let output_json = job.output.as_ref().map(serialize_json).transpose()?;

        conn.execute(
            "INSERT INTO jobs (id, job_type, status, priority, payload, progress, progress_message, output, error, worker_id, created_at, started_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                job.id.to_string(),
                job.job_type.as_str(),
                job.status.as_str(),
                job.priority,
                payload_json,
                job.progress,
                job.progress_message,
                output_json,
                job.error,
                job.worker_id,
                format_datetime(&job.created_at),
                job.started_at.as_ref().map(format_datetime),
                job.completed_at.as_ref().map(format_datetime),
            ],
        )
        .map_err(storage_err("failed to create job"))?;

        Ok(job.id)
    }

    fn job(&self, id: &Uuid) -> Result<Option<Job>> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT * FROM jobs WHERE id = ?1",
            params![id.to_string()],
            row_to_job,
        )
        .optional()
        .map_err(storage_err("failed to get job"))
    }

    fn update_job(&self, id: &Uuid, update: &JobUpdate) -> Result<()> {
        let conn = self.conn()?;
        let mut sets = Vec::new();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(status) = &update.status {
            param_values.push(Box::new(status.as_str().to_string()));
            sets.push(format!("status = ?{}", param_values.len()));
        }
        if let Some(progress) = &update.progress {
            param_values.push(Box::new(*progress));
            sets.push(format!("progress = ?{}", param_values.len()));
        }
        if let Some(ref msg) = update.progress_message {
            param_values.push(Box::new(msg.clone()));
            sets.push(format!("progress_message = ?{}", param_values.len()));
        }
        if let Some(ref output) = update.output {
            let json = output.as_ref().map(serialize_json).transpose()?;
            param_values.push(Box::new(json));
            sets.push(format!("output = ?{}", param_values.len()));
        }
        if let Some(ref error) = update.error {
            param_values.push(Box::new(error.clone()));
            sets.push(format!("error = ?{}", param_values.len()));
        }
        if let Some(ref worker) = update.worker_id {
            param_values.push(Box::new(worker.clone()));
            sets.push(format!("worker_id = ?{}", param_values.len()));
        }
        if let Some(ref started) = update.started_at {
            param_values.push(Box::new(started.as_ref().map(format_datetime)));
            sets.push(format!("started_at = ?{}", param_values.len()));
        }
        if let Some(ref completed) = update.completed_at {
            param_values.push(Box::new(completed.as_ref().map(format_datetime)));
            sets.push(format!("completed_at = ?{}", param_values.len()));
        }

        if sets.is_empty() {
            return Ok(());
        }

        param_values.push(Box::new(id.to_string()));
        let sql = format!(
            "UPDATE jobs SET {} WHERE id = ?{}",
            sets.join(", "),
            param_values.len()
        );

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = param_values
            .iter()
            .map(std::convert::AsRef::as_ref)
            .collect();

        conn.execute(&sql, param_refs.as_slice())
            .map_err(storage_err("failed to update job"))?;
        Ok(())
    }

    fn claim_next_job(&self, worker_id: &str) -> Result<Option<Job>> {
        let mut conn = self.conn()?;
        let now = format_datetime(&Utc::now());

        // Use IMMEDIATE transaction to prevent TOCTOU race between concurrent workers.
        // First SELECT the target id, then UPDATE by that specific id, then SELECT
        // it back. This avoids the previous approach where the post-UPDATE SELECT
        // filtered by worker_id+status, which could return the wrong job if the
        // worker already had another running job.
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(storage_err("failed to begin transaction"))?;

        let target_id: Option<String> = tx
            .query_row(
                "SELECT id FROM jobs WHERE status = 'pending' ORDER BY priority ASC, created_at ASC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_err("failed to find pending job"))?;

        let Some(target_id) = target_id else {
            tx.commit().map_err(storage_err("failed to commit claim"))?;
            return Ok(None);
        };

        tx.execute(
            "UPDATE jobs SET status = 'running', worker_id = ?1, started_at = ?2 WHERE id = ?3",
            params![worker_id, now, target_id],
        )
        .map_err(storage_err("failed to claim job"))?;

        let result = tx
            .query_row(
                "SELECT * FROM jobs WHERE id = ?1",
                params![target_id],
                row_to_job,
            )
            .optional()
            .map_err(storage_err("failed to get claimed job"))?;

        tx.commit().map_err(storage_err("failed to commit claim"))?;

        Ok(result)
    }

    fn claim_job_by_id(&self, job_id: &Uuid, worker_id: &str) -> Result<Option<Job>> {
        let mut conn = self.conn()?;
        let now = format_datetime(&Utc::now());
        let id_str = job_id.to_string();

        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(storage_err("failed to begin transaction"))?;

        let changed = tx
            .execute(
                "UPDATE jobs SET status = 'running', worker_id = ?1, started_at = ?2
                 WHERE id = ?3 AND status = 'pending'",
                params![worker_id, now, id_str],
            )
            .map_err(storage_err("failed to claim job by id"))?;

        let result = if changed == 0 {
            None
        } else {
            tx.query_row(
                "SELECT * FROM jobs WHERE id = ?1",
                params![id_str],
                row_to_job,
            )
            .optional()
            .map_err(storage_err("failed to get claimed job"))?
        };

        tx.commit().map_err(storage_err("failed to commit claim"))?;

        Ok(result)
    }

    fn list_jobs(&self, filters: &JobFilters) -> Result<Vec<Job>> {
        let conn = self.conn()?;
        let mut q = SqlQuery::new("SELECT * FROM jobs WHERE 1=1");

        if let Some(status) = filters.status {
            q.condition(" AND status = {}", status.as_str().to_string());
        }

        q.sql.push_str(" ORDER BY priority ASC, created_at DESC");

        q.paginate(filters.limit, filters.offset);

        let mut stmt = conn
            .prepare(&q.sql)
            .map_err(storage_err("failed to prepare list jobs query"))?;

        let jobs = stmt
            .query_map(q.param_refs().as_slice(), row_to_job)
            .map_err(storage_err("failed to list jobs"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect jobs"))?;

        Ok(jobs)
    }

    fn delete_jobs(&self, status: Option<JobStatus>) -> Result<u64> {
        let conn = self.conn()?;
        let count = match status {
            Some(s) => conn
                .execute("DELETE FROM jobs WHERE status = ?1", params![s.as_str()])
                .map_err(storage_err("failed to delete jobs"))?,
            None => conn
                .execute(
                    "DELETE FROM jobs WHERE status IN \
                     ('completed', 'failed', 'cancelled')",
                    [],
                )
                .map_err(storage_err("failed to delete jobs"))?,
        };
        Ok(count as u64)
    }

    fn prune_old_jobs(&self, policy: RetentionPolicy) -> Result<PruneReport> {
        if policy.is_disabled() {
            let conn = self.conn()?;
            let kept: u64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM jobs WHERE status IN ('completed','failed','cancelled')",
                    [],
                    |row| row.get(0),
                )
                .map_err(storage_err("failed to count jobs"))?;
            return Ok(PruneReport { deleted: 0, kept });
        }

        let conn = self.conn()?;
        let cutoff = policy.cutoff_str();
        let keep_last = policy.keep_last_i64();

        let deleted = conn
            .execute(
                "WITH ranked AS (
                    SELECT id,
                           ROW_NUMBER() OVER (ORDER BY COALESCE(completed_at, created_at) DESC) AS rn,
                           COALESCE(completed_at, created_at) AS effective_at
                    FROM jobs
                    WHERE status IN ('completed','failed','cancelled')
                )
                DELETE FROM jobs
                WHERE id IN (
                    SELECT id FROM ranked
                    WHERE (?1 IS NOT NULL AND effective_at < ?1)
                       OR (?2 IS NOT NULL AND rn > ?2)
                )",
                rusqlite::params![cutoff, keep_last],
            )
            .map_err(storage_err("failed to prune old jobs"))? as u64;

        let kept: u64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE status IN ('completed','failed','cancelled')",
                [],
                |row| row.get(0),
            )
            .map_err(storage_err("failed to count remaining jobs"))?;

        Ok(PruneReport { deleted, kept })
    }

    fn count_old_jobs(&self, policy: RetentionPolicy) -> Result<PruneReport> {
        if policy.is_disabled() {
            let conn = self.conn()?;
            let kept: u64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM jobs WHERE status IN ('completed','failed','cancelled')",
                    [],
                    |row| row.get(0),
                )
                .map_err(storage_err("failed to count jobs"))?;
            return Ok(PruneReport { deleted: 0, kept });
        }

        let conn = self.conn()?;
        let cutoff = policy.cutoff_str();
        let keep_last = policy.keep_last_i64();

        let deleted: u64 = conn
            .query_row(
                "WITH ranked AS (
                    SELECT id,
                           ROW_NUMBER() OVER (ORDER BY COALESCE(completed_at, created_at) DESC) AS rn,
                           COALESCE(completed_at, created_at) AS effective_at
                    FROM jobs
                    WHERE status IN ('completed','failed','cancelled')
                )
                SELECT COUNT(*) FROM ranked
                WHERE (?1 IS NOT NULL AND effective_at < ?1)
                   OR (?2 IS NOT NULL AND rn > ?2)",
                rusqlite::params![cutoff, keep_last],
                |row| row.get(0),
            )
            .map_err(storage_err("failed to count old jobs"))?;

        let total: u64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE status IN ('completed','failed','cancelled')",
                [],
                |row| row.get(0),
            )
            .map_err(storage_err("failed to count terminal jobs"))?;

        Ok(PruneReport {
            deleted,
            kept: total.saturating_sub(deleted),
        })
    }

    fn oldest_job_created_at(&self) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
        let conn = self.conn()?;
        let oldest: Option<String> = conn
            .query_row("SELECT MIN(created_at) FROM jobs", [], |row| row.get(0))
            .optional()
            .map_err(storage_err("failed to query oldest job"))?
            .flatten();

        oldest.map(|s| parse_datetime(&s)).transpose()
    }

    fn count_jobs_by_status(&self) -> Result<Vec<(JobStatus, u64)>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT status, COUNT(*) FROM jobs GROUP BY status")
            .map_err(storage_err("failed to prepare count query"))?;

        let counts = stmt
            .query_map([], |row| {
                let status_str: String = row.get(0)?;
                let count: i64 = row.get(1)?;
                Ok((status_str, count as u64))
            })
            .map_err(storage_err("failed to count jobs"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect counts"))?;

        let result = counts
            .into_iter()
            .map(|(s, c)| {
                JobStatus::parse(&s)
                    .map(|status| (status, c))
                    .ok_or_else(|| VoomError::Storage {
                        kind: StorageErrorKind::Other,
                        message: format!(
                            "unknown job status in database: '{s}' (count={c}) — data integrity issue"
                        ),
                    })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(result)
    }
}

use super::OptionalExt;

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::job::{Job, JobStatus, JobType, JobUpdate};
    use voom_domain::storage::{JobFilters, JobStorage};

    fn test_store() -> SqliteStore {
        SqliteStore::in_memory().expect("in-memory store")
    }

    #[test]
    fn create_and_fetch_job() {
        let store = test_store();
        let job = Job::new(JobType::Transcode);
        let id = store.create_job(&job).unwrap();
        assert_eq!(id, job.id);

        let fetched = store.job(&job.id).unwrap().unwrap();
        assert_eq!(fetched.id, job.id);
        assert_eq!(fetched.job_type, JobType::Transcode);
        assert_eq!(fetched.status, JobStatus::Pending);
    }

    #[test]
    fn job_unknown_returns_none() {
        let store = test_store();
        let missing = store.job(&Uuid::new_v4()).unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn update_job_multi_field() {
        let store = test_store();
        let job = Job::new(JobType::Process);
        store.create_job(&job).unwrap();

        let mut update = JobUpdate::default();
        update.status = Some(JobStatus::Running);
        update.progress = Some(0.42);
        update.progress_message = Some(Some("halfway".to_string()));
        update.worker_id = Some(Some("worker-1".to_string()));
        store.update_job(&job.id, &update).unwrap();

        let fetched = store.job(&job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Running);
        assert!((fetched.progress - 0.42).abs() < f64::EPSILON);
        assert_eq!(fetched.progress_message.as_deref(), Some("halfway"));
        assert_eq!(fetched.worker_id.as_deref(), Some("worker-1"));
    }

    #[test]
    fn update_job_empty_is_noop() {
        let store = test_store();
        let job = Job::new(JobType::Scan);
        store.create_job(&job).unwrap();

        let update = JobUpdate::default();
        store.update_job(&job.id, &update).unwrap();

        let fetched = store.job(&job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Pending);
        assert!(fetched.worker_id.is_none());
    }

    #[test]
    fn update_job_clear_optional_with_some_none() {
        let store = test_store();
        let mut job = Job::new(JobType::Transcode);
        job.worker_id = Some("worker-seed".to_string());
        job.error = Some("seed-error".to_string());
        store.create_job(&job).unwrap();

        let mut update = JobUpdate::default();
        update.worker_id = Some(None);
        update.error = Some(None);
        store.update_job(&job.id, &update).unwrap();

        let fetched = store.job(&job.id).unwrap().unwrap();
        assert!(fetched.worker_id.is_none());
        assert!(fetched.error.is_none());
    }

    #[test]
    fn claim_next_job_respects_priority() {
        let store = test_store();
        // Lower priority value = higher priority. Seed low-priority job first
        // so we can confirm ordering is not insertion-based.
        let mut low = Job::new(JobType::Scan);
        low.priority = 100;
        let mut high = Job::new(JobType::Transcode);
        high.priority = 10;
        store.create_job(&low).unwrap();
        store.create_job(&high).unwrap();

        let claimed = store.claim_next_job("worker-a").unwrap().unwrap();
        assert_eq!(claimed.id, high.id);
        assert_eq!(claimed.status, JobStatus::Running);
        assert_eq!(claimed.worker_id.as_deref(), Some("worker-a"));
    }

    #[test]
    fn claim_next_job_empty_queue_returns_none() {
        let store = test_store();
        let result = store.claim_next_job("worker-x").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn claim_job_by_id_non_pending_returns_none() {
        let store = test_store();
        let job = Job::new(JobType::Process);
        store.create_job(&job).unwrap();
        store.claim_next_job("worker-a").unwrap();

        // Job is now running — second attempt to claim by id should return None.
        let result = store.claim_job_by_id(&job.id, "worker-b").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn claim_job_by_id_unknown_returns_none() {
        let store = test_store();
        let result = store.claim_job_by_id(&Uuid::new_v4(), "worker-x").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn list_jobs_with_status_filter() {
        let store = test_store();
        let j1 = Job::new(JobType::Process);
        let j2 = Job::new(JobType::Scan);
        store.create_job(&j1).unwrap();
        store.create_job(&j2).unwrap();
        store.claim_next_job("worker-a").unwrap();

        let mut filters = JobFilters::default();
        filters.status = Some(JobStatus::Pending);
        let pending = store.list_jobs(&filters).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status, JobStatus::Pending);

        let mut running_filters = JobFilters::default();
        running_filters.status = Some(JobStatus::Running);
        let running = store.list_jobs(&running_filters).unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].status, JobStatus::Running);
    }

    #[test]
    fn list_jobs_limit_and_offset() {
        let store = test_store();
        for _ in 0..5 {
            let job = Job::new(JobType::Scan);
            store.create_job(&job).unwrap();
        }
        let mut filters = JobFilters::default();
        filters.limit = Some(2);
        filters.offset = Some(1);
        let results = store.list_jobs(&filters).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn count_jobs_by_status_groups_correctly() {
        let store = test_store();
        let job_a = Job::new(JobType::Process);
        let job_b = Job::new(JobType::Process);
        store.create_job(&job_a).unwrap();
        store.create_job(&job_b).unwrap();
        store.claim_next_job("worker-a").unwrap();

        let counts = store.count_jobs_by_status().unwrap();
        let map: std::collections::HashMap<JobStatus, u64> = counts.into_iter().collect();
        assert_eq!(map.get(&JobStatus::Pending).copied(), Some(1));
        assert_eq!(map.get(&JobStatus::Running).copied(), Some(1));
    }

    #[test]
    fn delete_jobs_with_specific_status() {
        let store = test_store();
        let job = Job::new(JobType::Process);
        store.create_job(&job).unwrap();
        let mut update = JobUpdate::default();
        update.status = Some(JobStatus::Completed);
        store.update_job(&job.id, &update).unwrap();

        let deleted = store.delete_jobs(Some(JobStatus::Completed)).unwrap();
        assert_eq!(deleted, 1);
        assert!(store.job(&job.id).unwrap().is_none());
    }

    use voom_domain::storage::RetentionPolicy;

    fn insert_test_job(
        store: &SqliteStore,
        status: voom_domain::job::JobStatus,
        completed_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> uuid::Uuid {
        let mut job = voom_domain::job::Job::new(voom_domain::job::JobType::Introspect);
        job.status = status;
        job.completed_at = completed_at;
        store.create_job(&job).unwrap();
        job.id
    }

    #[test]
    fn prune_old_jobs_disabled_policy_is_noop() {
        let store = SqliteStore::in_memory().unwrap();
        insert_test_job(
            &store,
            voom_domain::job::JobStatus::Completed,
            Some(chrono::Utc::now() - chrono::Duration::days(365)),
        );
        let report = store.prune_old_jobs(RetentionPolicy::default()).unwrap();
        assert_eq!(report.deleted, 0);
        assert_eq!(report.kept, 1);
    }

    #[test]
    fn prune_old_jobs_respects_terminal_status_only() {
        let store = SqliteStore::in_memory().unwrap();
        insert_test_job(&store, voom_domain::job::JobStatus::Pending, None);
        insert_test_job(&store, voom_domain::job::JobStatus::Running, None);
        let policy = RetentionPolicy {
            max_age: Some(chrono::Duration::seconds(0)),
            keep_last: None,
        };
        let report = store.prune_old_jobs(policy).unwrap();
        assert_eq!(
            report.deleted, 0,
            "pending/running rows must never be deleted"
        );
        assert_eq!(report.kept, 0, "kept counts only eligible rows");
    }

    #[test]
    fn prune_old_jobs_age_only_deletes_old() {
        let store = SqliteStore::in_memory().unwrap();
        let now = chrono::Utc::now();
        insert_test_job(
            &store,
            voom_domain::job::JobStatus::Completed,
            Some(now - chrono::Duration::days(30)),
        );
        insert_test_job(
            &store,
            voom_domain::job::JobStatus::Completed,
            Some(now - chrono::Duration::hours(1)),
        );
        let policy = RetentionPolicy {
            max_age: Some(chrono::Duration::days(7)),
            keep_last: None,
        };
        let report = store.prune_old_jobs(policy).unwrap();
        assert_eq!(report.deleted, 1);
        assert_eq!(report.kept, 1);
    }

    #[test]
    fn prune_old_jobs_count_only_keeps_newest() {
        let store = SqliteStore::in_memory().unwrap();
        let now = chrono::Utc::now();
        for i in 0..5i64 {
            insert_test_job(
                &store,
                voom_domain::job::JobStatus::Completed,
                Some(now - chrono::Duration::minutes(i)),
            );
        }
        let policy = RetentionPolicy {
            max_age: None,
            keep_last: Some(2),
        };
        let report = store.prune_old_jobs(policy).unwrap();
        assert_eq!(report.deleted, 3);
        assert_eq!(report.kept, 2);
    }

    #[test]
    fn prune_old_jobs_or_semantics_combines_bounds() {
        let store = SqliteStore::in_memory().unwrap();
        let now = chrono::Utc::now();
        // 5 rows, age in days: [10, 5, 3, 1, 0]
        for days in [10i64, 5, 3, 1, 0] {
            insert_test_job(
                &store,
                voom_domain::job::JobStatus::Completed,
                Some(now - chrono::Duration::days(days)),
            );
        }
        let policy = RetentionPolicy {
            max_age: Some(chrono::Duration::days(4)), // deletes 10d, 5d
            keep_last: Some(3),                       // would also delete the 4th-newest (5d)
        };
        let report = store.prune_old_jobs(policy).unwrap();
        // OR semantics: 10d (too old), 5d (too old AND beyond rank 3), nothing else qualifies
        assert_eq!(report.deleted, 2);
        assert_eq!(report.kept, 3);
    }

    #[test]
    fn prune_old_jobs_completed_at_fallback_to_created_at() {
        let store = SqliteStore::in_memory().unwrap();
        // A failed job with no completed_at — falls back to created_at (defaults to now in Job::new)
        insert_test_job(&store, voom_domain::job::JobStatus::Failed, None);
        // Make policy aggressively short so created_at (≈now) is NOT older
        let policy = RetentionPolicy {
            max_age: Some(chrono::Duration::days(1)),
            keep_last: None,
        };
        let report = store.prune_old_jobs(policy).unwrap();
        assert_eq!(
            report.deleted, 0,
            "row younger than max_age via created_at fallback"
        );
    }

    #[test]
    fn prune_old_jobs_empty_table_is_noop() {
        let store = SqliteStore::in_memory().unwrap();
        let policy = RetentionPolicy {
            max_age: Some(chrono::Duration::days(1)),
            keep_last: Some(10),
        };
        let report = store.prune_old_jobs(policy).unwrap();
        assert_eq!(report.deleted, 0);
        assert_eq!(report.kept, 0);
    }

    #[test]
    fn oldest_job_created_at_returns_min_on_sqlite() {
        use voom_domain::job::{Job, JobType};
        let store = test_store();
        let mut older = Job::new(JobType::Process);
        older.created_at = chrono::Utc::now() - chrono::Duration::days(3);
        store.create_job(&older).unwrap();
        let mut newer = Job::new(JobType::Process);
        newer.created_at = chrono::Utc::now();
        store.create_job(&newer).unwrap();

        let got = store.oldest_job_created_at().unwrap().unwrap();
        assert_eq!(got.timestamp_millis(), older.created_at.timestamp_millis());
    }

    #[test]
    fn count_old_jobs_matches_prune_count() {
        let store = SqliteStore::in_memory().unwrap();
        let now = chrono::Utc::now();
        for days in [10i64, 5, 3, 1, 0] {
            insert_test_job(
                &store,
                voom_domain::job::JobStatus::Completed,
                Some(now - chrono::Duration::days(days)),
            );
        }
        let policy = RetentionPolicy {
            max_age: Some(chrono::Duration::days(4)),
            keep_last: Some(3),
        };
        let count_report = store.count_old_jobs(policy).unwrap();
        // Same data still in store (count is non-destructive): verify by counting
        let total_before: u64 = store
            .conn()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE status IN ('completed','failed','cancelled')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            total_before, 5,
            "count_old_jobs must not modify the database"
        );
        let prune_report = store.prune_old_jobs(policy).unwrap();
        assert_eq!(count_report.deleted, prune_report.deleted);
        assert_eq!(count_report.kept, prune_report.kept);
    }

    #[test]
    fn delete_jobs_all_terminal_keeps_pending_running() {
        let store = test_store();
        let pending = Job::new(JobType::Process);
        let running = Job::new(JobType::Transcode);
        let completed = Job::new(JobType::Scan);
        let failed = Job::new(JobType::Introspect);
        store.create_job(&pending).unwrap();
        store.create_job(&running).unwrap();
        store.create_job(&completed).unwrap();
        store.create_job(&failed).unwrap();

        let set_status = |id, status| {
            let mut u = JobUpdate::default();
            u.status = Some(status);
            store.update_job(id, &u).unwrap();
        };
        set_status(&running.id, JobStatus::Running);
        set_status(&completed.id, JobStatus::Completed);
        set_status(&failed.id, JobStatus::Failed);

        let deleted = store.delete_jobs(None).unwrap();
        assert_eq!(deleted, 2, "should delete only completed + failed");

        assert!(store.job(&pending.id).unwrap().is_some());
        assert!(store.job(&running.id).unwrap().is_some());
        assert!(store.job(&completed.id).unwrap().is_none());
        assert!(store.job(&failed.id).unwrap().is_none());
    }
}
