use chrono::Utc;
use rusqlite::params;
use uuid::Uuid;

use voom_domain::errors::Result;
use voom_domain::plan::Plan;
use voom_domain::storage::{PlanStatus, PlanStorage, StoredPlan};

use super::{
    format_datetime, other_storage_err, parse_optional_datetime, row_uuid, storage_err,
    OptionalExt, SqliteStore,
};

impl PlanStorage for SqliteStore {
    fn save_plan(&self, plan: &Plan) -> Result<Uuid> {
        let conn = self.conn()?;
        let actions_json = serde_json::to_string(&plan.actions)
            .map_err(other_storage_err("failed to serialize actions"))?;
        let warnings_json = if plan.warnings.is_empty() {
            None
        } else {
            Some(
                serde_json::to_string(&plan.warnings)
                    .map_err(other_storage_err("failed to serialize warnings"))?,
            )
        };

        // Resolve file_id by path to handle ID preservation in upsert_file.
        // When a file is re-scanned, upsert_file keeps the original DB ID, but
        // the Plan's file.id may be a fresh UUID from the new introspection.
        let path_str = plan.file.path.to_string_lossy().to_string();
        let effective_file_id: String = conn
            .query_row(
                "SELECT id FROM files WHERE path = ?1",
                params![&path_str],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_err("failed to resolve file id"))?
            .unwrap_or_else(|| plan.file.id.to_string());

        conn.execute(
            "INSERT INTO plans (id, file_id, policy_name, phase_name, status, actions, warnings, skip_reason, policy_hash, evaluated_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                plan.id.to_string(),
                effective_file_id,
                plan.policy_name,
                plan.phase_name,
                PlanStatus::Pending.as_str(),
                actions_json,
                warnings_json,
                plan.skip_reason,
                plan.policy_hash,
                format_datetime(&plan.evaluated_at),
                format_datetime(&Utc::now()),
            ],
        )
        .map_err(storage_err("failed to save plan"))?;

        Ok(plan.id)
    }

    fn update_plan_status(&self, plan_id: &Uuid, status: PlanStatus) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE plans SET status = ?1, executed_at = ?2 WHERE id = ?3",
            params![
                status.as_str(),
                format_datetime(&Utc::now()),
                plan_id.to_string()
            ],
        )
        .map_err(storage_err("failed to update plan status"))?;
        Ok(())
    }

    fn get_plans_for_file(&self, file_id: &Uuid) -> Result<Vec<StoredPlan>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, file_id, policy_name, phase_name, status, actions, warnings, skip_reason, policy_hash, evaluated_at, created_at, executed_at, result
                 FROM plans WHERE file_id = ?1 ORDER BY created_at",
            )
            .map_err(storage_err("failed to prepare plans query"))?;

        let plans = stmt
            .query_map(params![file_id.to_string()], |row| {
                let id_str: String = row.get("id")?;
                let file_id_str: String = row.get("file_id")?;
                Ok(StoredPlan {
                    id: row_uuid(&id_str, "plans")?,
                    file_id: row_uuid(&file_id_str, "plans")?,
                    policy_name: row.get("policy_name")?,
                    phase_name: row.get("phase_name")?,
                    status: {
                        let s: String = row.get("status")?;
                        PlanStatus::parse(&s).ok_or_else(|| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                format!("unknown plan status: {s}").into(),
                            )
                        })?
                    },
                    actions_json: row.get("actions")?,
                    warnings: row.get("warnings")?,
                    skip_reason: row.get("skip_reason")?,
                    policy_hash: row.get("policy_hash")?,
                    evaluated_at: parse_optional_datetime(
                        row.get("evaluated_at")?,
                        "plans.evaluated_at",
                    )?,
                    created_at: {
                        let s: String = row.get("created_at")?;
                        s.parse().map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                format!("invalid datetime in plans.created_at: {e}").into(),
                            )
                        })?
                    },
                    executed_at: parse_optional_datetime(
                        row.get("executed_at")?,
                        "plans.executed_at",
                    )?,
                    result: row.get("result")?,
                })
            })
            .map_err(storage_err("failed to query plans"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect plans"))?;

        Ok(plans)
    }
}
