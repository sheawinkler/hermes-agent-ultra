use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{CronSchedule, TaskId, UserId, VerticalId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    pub owner_user_id: UserId,
    pub vertical: Option<VerticalId>,
    pub title: String,
    pub prompt_template: String,
    pub schedule: CronSchedule,
    pub enabled: bool,
    pub last_task_id: Option<TaskId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl CronJob {
    pub fn new(
        owner_user_id: UserId,
        title: impl Into<String>,
        prompt_template: impl Into<String>,
        schedule: CronSchedule,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: ulid::Ulid::new().to_string(),
            owner_user_id,
            vertical: None,
            title: title.into(),
            prompt_template: prompt_template.into(),
            schedule,
            enabled: true,
            last_task_id: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn w16_cron_types_new_job_has_id() {
        let owner = UserId::new();
        let schedule = CronSchedule {
            expr: "@daily".into(),
            timezone: "Asia/Shanghai".into(),
            next_run: None,
            last_run: None,
            enabled: true,
        };
        let job = CronJob::new(owner, "brief", "hello", schedule);
        assert!(!job.id.is_empty());
        assert_eq!(job.title, "brief");
    }
}
