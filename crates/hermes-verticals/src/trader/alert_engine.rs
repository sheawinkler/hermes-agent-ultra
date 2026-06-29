use tracing::info;

use super::watchlist::Watchlist;

pub fn check_watchlist_alerts(list: &Watchlist) -> Vec<String> {
    let mut triggered = Vec::new();
    for rule in &list.alert_rules {
        info!(kind = %rule.kind, threshold = rule.threshold, "alert rule check");
        triggered.push(format!("{}:{}", rule.kind, rule.threshold));
    }
    triggered
}
