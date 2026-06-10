//! Cron result delivery (Python `cron.scheduler._deliver_result` parity).

use std::time::Instant;

use async_trait::async_trait;

use crate::job::{CronJob, DeliverConfig, DeliverTarget};
use crate::python_job::JobOrigin;
use crate::timing::log_job_delivery;

/// Resolved IM delivery target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDelivery {
    pub platform: String,
    pub chat_id: String,
}

/// Backend for sending cron output to a platform (wired to gateway in `hermes-cli`).
#[async_trait]
pub trait CronDeliveryBackend: Send + Sync {
    async fn send(&self, platform: &str, chat_id: &str, message: &str) -> Result<(), String>;
}

/// Environment variable for platform home channel (Python `_HOME_TARGET_ENV_VARS`).
pub fn home_channel_env_for_platform(platform: &str) -> Option<&'static str> {
    match platform.to_ascii_lowercase().as_str() {
        "telegram" => Some("TELEGRAM_HOME_CHANNEL"),
        "discord" => Some("DISCORD_HOME_CHANNEL"),
        "slack" => Some("SLACK_HOME_CHANNEL"),
        "signal" => Some("SIGNAL_HOME_CHANNEL"),
        "mattermost" => Some("MATTERMOST_HOME_CHANNEL"),
        "sms" => Some("SMS_HOME_CHANNEL"),
        "email" => Some("EMAIL_HOME_ADDRESS"),
        "dingtalk" => Some("DINGTALK_HOME_CHANNEL"),
        "feishu" => Some("FEISHU_HOME_CHANNEL"),
        "wecom" => Some("WECOM_HOME_CHANNEL"),
        "weixin" | "wechat" | "wx" => Some("WEIXIN_HOME_CHANNEL"),
        "matrix" => Some("MATRIX_HOME_ROOM"),
        "bluebubbles" => Some("BLUEBUBBLES_HOME_CHANNEL"),
        "whatsapp" => Some("WHATSAPP_HOME_CHANNEL"),
        _ => None,
    }
}

fn home_channel_from_env(platform: &str) -> Option<String> {
    let key = home_channel_env_for_platform(platform)?;
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn platform_name_for_target(target: &DeliverTarget) -> Option<&'static str> {
    match target {
        DeliverTarget::Telegram => Some("telegram"),
        DeliverTarget::Discord => Some("discord"),
        DeliverTarget::Slack => Some("slack"),
        DeliverTarget::Email => Some("email"),
        DeliverTarget::WhatsApp => Some("whatsapp"),
        DeliverTarget::Signal => Some("signal"),
        DeliverTarget::Matrix => Some("matrix"),
        DeliverTarget::Mattermost => Some("mattermost"),
        DeliverTarget::DingTalk => Some("dingtalk"),
        DeliverTarget::Feishu => Some("feishu"),
        DeliverTarget::WeCom => Some("wecom"),
        DeliverTarget::Weixin => Some("weixin"),
        DeliverTarget::BlueBubbles => Some("bluebubbles"),
        DeliverTarget::Sms => Some("sms"),
        DeliverTarget::Ntfy => Some("ntfy"),
        DeliverTarget::Origin | DeliverTarget::Local | DeliverTarget::HomeAssistant => None,
    }
}

/// Default deliver when unset (Python `create_job`).
pub fn default_deliver_for_job(origin: &Option<JobOrigin>) -> DeliverConfig {
    if origin.is_some() {
        DeliverConfig {
            target: DeliverTarget::Origin,
            platform: None,
        }
    } else {
        DeliverConfig {
            target: DeliverTarget::Local,
            platform: None,
        }
    }
}

/// Effective deliver config for a job.
pub fn effective_deliver(job: &CronJob) -> DeliverConfig {
    job.deliver
        .clone()
        .unwrap_or_else(|| default_deliver_for_job(&job.origin))
}

/// Resolve delivery targets (Python `_resolve_delivery_target` subset).
pub fn resolve_delivery_targets(job: &CronJob) -> Vec<ResolvedDelivery> {
    let deliver = effective_deliver(job);
    resolve_deliver_config(job, &deliver)
}

fn resolve_deliver_config(job: &CronJob, deliver: &DeliverConfig) -> Vec<ResolvedDelivery> {
    match deliver.target {
        DeliverTarget::Origin => {
            if let Some(origin) = job.origin.as_ref() {
                if let Some(chat_id) = origin.chat_id.as_deref().filter(|s| !s.is_empty()) {
                    return vec![ResolvedDelivery {
                        platform: origin.platform.clone(),
                        chat_id: chat_id.to_string(),
                    }];
                }
            }
            for platform in [
                "whatsapp", "wecom", "weixin", "telegram", "discord", "slack", "feishu",
                "dingtalk",
            ] {
                if let Some(chat_id) = home_channel_from_env(platform) {
                    return vec![ResolvedDelivery {
                        platform: platform.to_string(),
                        chat_id,
                    }];
                }
            }
            vec![]
        }
        DeliverTarget::Local => vec![],
        _ => {
            let Some(platform) = platform_name_for_target(&deliver.target) else {
                return vec![];
            };
            let Some(chat_id) = deliver
                .platform
                .clone()
                .filter(|s| !s.is_empty())
                .or_else(|| home_channel_from_env(platform))
            else {
                return vec![];
            };
            vec![ResolvedDelivery {
                platform: platform.to_string(),
                chat_id,
            }]
        }
    }
}

/// Best-effort deliver text to resolved targets.
pub async fn deliver_text(
    backend: &dyn CronDeliveryBackend,
    job: &CronJob,
    text: &str,
) -> Option<String> {
    let deliver = effective_deliver(job);
    match deliver.target {
        DeliverTarget::Local => {
            tracing::info!("Cron job result (local delivery):\n{}", text);
            None
        }
        DeliverTarget::Origin if resolve_delivery_targets(job).is_empty() => {
            tracing::warn!(
                "Cron job '{}' has deliver=origin but no origin/home channel configured",
                job.id
            );
            Some("no delivery target configured".into())
        }
        _ => {
            let targets = resolve_delivery_targets(job);
            if targets.is_empty() {
                tracing::warn!(
                    "Cron job '{}' has no resolvable delivery target for {:?}",
                    job.id,
                    deliver.target
                );
                return Some("no delivery target configured".into());
            }
            let mut last_err = None;
            for t in targets {
                log_job_delivery(&job.id, &t.platform, &t.chat_id, "start", None, None);
                let send_started = Instant::now();
                match backend.send(&t.platform, &t.chat_id, text).await {
                    Ok(()) => {
                        let elapsed_ms =
                            i64::try_from(send_started.elapsed().as_millis()).unwrap_or(i64::MAX);
                        log_job_delivery(
                            &job.id,
                            &t.platform,
                            &t.chat_id,
                            "finish",
                            Some(elapsed_ms),
                            None,
                        );
                    }
                    Err(e) => {
                        let elapsed_ms =
                            i64::try_from(send_started.elapsed().as_millis()).unwrap_or(i64::MAX);
                        log_job_delivery(
                            &job.id,
                            &t.platform,
                            &t.chat_id,
                            "finish",
                            Some(elapsed_ms),
                            Some(&e),
                        );
                        last_err = Some(e);
                    }
                }
            }
            last_err
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::CronJob;
    use crate::python_job::JobOrigin;

    #[test]
    fn resolve_wecom_deliver_with_explicit_chat() {
        let job = CronJob {
            deliver: Some(DeliverConfig {
                target: DeliverTarget::WeCom,
                platform: Some("chat-abc".into()),
            }),
            ..CronJob::new("every 2h", "hi")
        };
        let targets = resolve_delivery_targets(&job);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].platform, "wecom");
        assert_eq!(targets[0].chat_id, "chat-abc");
    }

    #[test]
    fn default_deliver_uses_origin_when_present() {
        let mut job = CronJob::new("every 2h", "hi");
        job.origin = Some(JobOrigin {
            platform: "wecom".into(),
            chat_id: Some("gid".into()),
            thread_id: None,
        });
        let d = effective_deliver(&job);
        assert_eq!(d.target, DeliverTarget::Origin);
        let targets = resolve_delivery_targets(&job);
        assert_eq!(targets[0].chat_id, "gid");
    }
}
