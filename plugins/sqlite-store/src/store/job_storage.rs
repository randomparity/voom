use chrono::Utc;
use rusqlite::params;
use uuid::Uuid;

use voom_domain::errors::{Result, VoomError};
use voom_domain::job::{Job, JobStatus, JobUpdate};
use voom_domain::storage::{JobFilters, JobStorage};

use super::{format_datetime, row_to_job, storage_err, SqlQuery, SqliteStore};

fn serialize_json(value: &serde_json::Value) -> Result<String> {
    serde_json::to_string(value).map_err(super::storage_err("failed to serialize JSON"))
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
                job.job_type,
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

    fn get_job(&self, id: &Uuid) -> Result<Option<Job>> {
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

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|v| v.as_ref()).collect();

        conn.execute(&sql, param_refs.as_slice())
            .map_err(storage_err("failed to update job"))?;
        Ok(())
    }

    fn claim_next_job(&self, worker_id: &str) -> Result<Option<Job>> {
        let mut conn = self.conn()?;
        let now = format_datetime(&Utc::now());

        // Use IMMEDIATE transaction to prevent TOCTOU race between concurrent workers
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(storage_err("failed to begin transaction"))?;

        tx.execute(
            "UPDATE jobs SET status = 'running', worker_id = ?1, started_at = ?2
             WHERE id = (SELECT id FROM jobs WHERE status = 'pending' ORDER BY priority ASC, created_at ASC LIMIT 1)",
            params![worker_id, now],
        )
        .map_err(storage_err("failed to claim job"))?;

        let result = tx
            .query_row(
                "SELECT * FROM jobs WHERE worker_id = ?1 AND status = 'running' ORDER BY started_at DESC LIMIT 1",
                params![worker_id],
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

        if let Some(limit) = filters.limit {
            q.condition(" LIMIT {}", limit.min(10_000).to_string());
        }
        if let Some(offset) = filters.offset {
            q.condition(" OFFSET {}", offset.min(1_000_000).to_string());
        }

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
                    .ok_or_else(|| {
                        VoomError::Storage(format!(
                            "unknown job status in database: '{s}' (count={c}) — data integrity issue"
                        ))
                    })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(result)
    }
}

use super::OptionalExt;
