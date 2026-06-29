pub fn approval_deep_link(task_id: &str, event_id: &str) -> String {
    format!("terra://tasks/{task_id}/approval/{event_id}")
}

pub fn should_notify_task_done(duration_secs: u64) -> bool {
    duration_secs >= 300
}

pub async fn notify_approval(task_id: &str, event_id: &str) -> Result<(), String> {
    let _link = approval_deep_link(task_id, event_id);
    Ok(())
}

pub async fn notify_task_done(task_id: &str, duration_secs: u64) -> Result<(), String> {
    if !should_notify_task_done(duration_secs) {
        return Ok(());
    }
    let _ = task_id;
    Ok(())
}
