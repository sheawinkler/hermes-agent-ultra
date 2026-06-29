use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use tokio::sync::Mutex;
use tracing::info;

use super::types::CronJob;
use crate::types::{CronSchedule, UserId};

#[derive(Clone, Default)]
pub struct CronRuntime {
    jobs: Arc<Mutex<HashMap<String, CronJob>>>,
}

impl CronRuntime {
    pub fn new() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn upsert(&self, job: CronJob) {
        self.jobs.lock().await.insert(job.id.clone(), job);
    }

    pub async fn list_all(&self) -> Vec<CronJob> {
        self.jobs.lock().await.values().cloned().collect()
    }

    pub async fn list_for_user(&self, owner: UserId) -> Vec<CronJob> {
        self.jobs
            .lock()
            .await
            .values()
            .filter(|j| j.owner_user_id == owner)
            .cloned()
            .collect()
    }

    pub async fn get(&self, id: &str) -> Option<CronJob> {
        self.jobs.lock().await.get(id).cloned()
    }

    pub async fn delete(&self, id: &str) -> bool {
        self.jobs.lock().await.remove(id).is_some()
    }

    pub async fn tick(&self) {
        let mut due: Vec<CronJob> = Vec::new();
        {
            let mut guard = self.jobs.lock().await;
            for job in guard.values_mut() {
                if !job.enabled {
                    continue;
                }
                if schedule_due(&job.schedule) {
                    job.schedule.last_run = Some(Utc::now());
                    due.push(job.clone());
                }
            }
        }
        for job in due {
            info!(job_id = %job.id, title = %job.title, "cron job due");
        }
    }
}

fn schedule_due(schedule: &CronSchedule) -> bool {
    schedule
        .next_run
        .map(|next| next <= Utc::now())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CronSchedule;

    fn sample_job(owner: UserId) -> CronJob {
        let schedule = CronSchedule {
            expr: "0 * * * *".into(),
            timezone: "UTC".into(),
            next_run: None,
            last_run: None,
            enabled: true,
        };
        CronJob::new(owner, "t", "p", schedule)
    }

    #[tokio::test]
    async fn w16_cron_runtime_upsert_list_delete() {
        let rt = CronRuntime::new();
        let owner = UserId::new();
        let job = sample_job(owner);
        let id = job.id.clone();
        rt.upsert(job).await;
        assert_eq!(rt.list_for_user(owner).await.len(), 1);
        assert!(rt.get(&id).await.is_some());
        assert!(rt.delete(&id).await);
        assert!(rt.get(&id).await.is_none());
    }
}
