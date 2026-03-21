//! Re-export the shared InMemoryStore from voom-domain.
//!
//! The canonical implementation lives in `voom_domain::test_support::InMemoryStore`.
//! This module re-exports it so that existing test code continues to compile unchanged.

pub use voom_domain::test_support::InMemoryStore;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use uuid::Uuid;
    use voom_domain::job::{Job, JobStatus, JobUpdate};
    use voom_domain::media::MediaFile;
    use voom_domain::plan::Plan;
    use voom_domain::stats::ProcessingStats;
    use voom_domain::storage::{
        BadFileStorage, FileFilters, FileHistoryStorage, FileStorage, JobFilters, JobStorage,
        MaintenanceStorage, PlanStorage, PluginDataStorage, StatsStorage,
    };

    fn make_job(job_type: &str, priority: i32) -> Job {
        let mut job = Job::new(job_type.to_string());
        job.priority = priority;
        job
    }

    #[test]
    fn create_and_get_job() {
        let store = InMemoryStore::new();
        let job = make_job("test", 0);
        let id = job.id;
        store.create_job(&job).unwrap();
        let fetched = store.get_job(&id).unwrap().unwrap();
        assert_eq!(fetched.id, id);
        assert_eq!(fetched.job_type, "test");
    }

    #[test]
    fn get_nonexistent_job_returns_none() {
        let store = InMemoryStore::new();
        let result = store.get_job(&Uuid::new_v4()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn update_job_status() {
        let store = InMemoryStore::new();
        let job = make_job("test", 0);
        let id = job.id;
        store.create_job(&job).unwrap();

        let update = JobUpdate {
            status: Some(JobStatus::Running),
            progress: Some(0.5),
            progress_message: None,
            output: None,
            error: None,
            worker_id: Some(Some("w1".to_string())),
            started_at: None,
            completed_at: None,
        };
        store.update_job(&id, &update).unwrap();

        let fetched = store.get_job(&id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Running);
        assert!((fetched.progress - 0.5).abs() < f64::EPSILON);
        assert_eq!(fetched.worker_id, Some("w1".to_string()));
    }

    #[test]
    fn update_nonexistent_job_errors() {
        let store = InMemoryStore::new();
        let update = JobUpdate {
            status: Some(JobStatus::Failed),
            ..Default::default()
        };
        let result = store.update_job(&Uuid::new_v4(), &update);
        assert!(result.is_err());
    }

    #[test]
    fn claim_next_job_picks_highest_priority() {
        let store = InMemoryStore::new();
        let low = make_job("low", 10);
        let high = make_job("high", 1);
        store.create_job(&low).unwrap();
        store.create_job(&high).unwrap();

        let claimed = store.claim_next_job("worker-1").unwrap().unwrap();
        assert_eq!(claimed.job_type, "high");
        assert_eq!(claimed.status, JobStatus::Running);
        assert_eq!(claimed.worker_id, Some("worker-1".to_string()));
    }

    #[test]
    fn claim_next_job_returns_none_when_empty() {
        let store = InMemoryStore::new();
        let result = store.claim_next_job("w1").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn claim_next_job_skips_running_jobs() {
        let store = InMemoryStore::new();
        let job = make_job("test", 0);
        let id = job.id;
        store.create_job(&job).unwrap();
        store.claim_next_job("w1").unwrap(); // claims it

        // No more pending jobs
        let result = store.claim_next_job("w2").unwrap();
        assert!(result.is_none());

        // Verify the job is running
        let fetched = store.get_job(&id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Running);
    }

    #[test]
    fn list_jobs_filters_by_status() {
        let store = InMemoryStore::new();
        let j1 = make_job("a", 0);
        let j2 = make_job("b", 0);
        store.create_job(&j1).unwrap();
        store.create_job(&j2).unwrap();
        store.claim_next_job("w1").unwrap(); // one becomes Running

        let pending = store
            .list_jobs(&JobFilters {
                status: Some(JobStatus::Pending),
                limit: None,
            })
            .unwrap();
        assert_eq!(pending.len(), 1);

        let running = store
            .list_jobs(&JobFilters {
                status: Some(JobStatus::Running),
                limit: None,
            })
            .unwrap();
        assert_eq!(running.len(), 1);

        let all = store.list_jobs(&JobFilters::default()).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn list_jobs_respects_limit() {
        let store = InMemoryStore::new();
        for i in 0..5 {
            store.create_job(&make_job(&format!("j{i}"), i)).unwrap();
        }
        let limited = store
            .list_jobs(&JobFilters {
                status: None,
                limit: Some(3),
            })
            .unwrap();
        assert_eq!(limited.len(), 3);
    }

    #[test]
    fn count_jobs_by_status_counts_correctly() {
        let store = InMemoryStore::new();
        store.create_job(&make_job("a", 0)).unwrap();
        store.create_job(&make_job("b", 1)).unwrap();
        store.create_job(&make_job("c", 2)).unwrap();
        store.claim_next_job("w1").unwrap();

        let counts = store.count_jobs_by_status().unwrap();
        let map: HashMap<JobStatus, u64> = counts.into_iter().collect();
        assert_eq!(map.get(&JobStatus::Pending), Some(&2));
        assert_eq!(map.get(&JobStatus::Running), Some(&1));
    }

    #[test]
    fn file_methods_work() {
        let store = InMemoryStore::new();
        let file = MediaFile::new(PathBuf::from("/x"));

        // File operations are fully functional
        assert!(store.upsert_file(&file).is_ok());
        assert!(store.get_file(&file.id).unwrap().is_some());
        assert!(store.get_file_by_path(Path::new("/x")).unwrap().is_some());
        assert_eq!(store.list_files(&FileFilters::default()).unwrap().len(), 1);
        assert!(store.delete_file(&file.id).is_ok());
        assert!(store.get_file(&file.id).unwrap().is_none());

        // Non-existent lookups return None
        assert!(store.get_file(&Uuid::new_v4()).unwrap().is_none());
    }

    #[test]
    fn stub_methods_return_defaults() {
        let store = InMemoryStore::new();
        let file = MediaFile::new(PathBuf::from("/x"));

        let plan = Plan {
            id: Uuid::new_v4(),
            file: file.clone(),
            policy_name: "test".into(),
            phase_name: "phase1".into(),
            actions: vec![],
            warnings: vec![],
            skip_reason: None,
            policy_hash: None,
            evaluated_at: chrono::Utc::now(),
        };
        assert!(store.save_plan(&plan).unwrap() != Uuid::nil());
        assert!(store
            .get_plans_for_file(&Uuid::new_v4())
            .unwrap()
            .is_empty());
        assert!(store
            .update_plan_status(&Uuid::new_v4(), voom_domain::storage::PlanStatus::Completed)
            .is_ok());
        assert!(store.get_file_history(Path::new("/x")).unwrap().is_empty());

        let stats = ProcessingStats::new(file.id, "test".into(), "phase1".into());
        assert!(store.record_stats(&stats).is_ok());
        assert!(store.get_plugin_data("p", "k").unwrap().is_none());
        assert!(store.set_plugin_data("p", "k", b"v").is_ok());
        assert!(store.vacuum().is_ok());
        assert_eq!(store.prune_missing_files().unwrap(), 0);
    }
}
