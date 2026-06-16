//! Connectivity and configuration diagnostics for the remote LLM server.

use hermes_config::ServerConfig;

use crate::auth::AuthManager;
use crate::session::TokenSource;

#[derive(Debug, Clone)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}

#[derive(Debug, Clone)]
pub struct DoctorCheck {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

impl DoctorReport {
    pub fn all_ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }

    pub fn print_lines(&self) -> Vec<String> {
        self.checks
            .iter()
            .map(|c| {
                let mark = if c.ok { "ok" } else { "FAIL" };
                format!("[{mark}] {} — {}", c.name, c.detail)
            })
            .collect()
    }
}

pub async fn run_doctor(
    config: &ServerConfig,
    hermes_home: impl AsRef<std::path::Path>,
) -> DoctorReport {
    let mut checks = Vec::new();

    checks.push(DoctorCheck {
        name: "server.enabled",
        ok: config.enabled,
        detail: if config.enabled {
            "server integration enabled".to_string()
        } else {
            "server integration disabled (set server.enabled or HERMES_SERVER_ENABLED)".to_string()
        },
    });

    let base_ok = config.api_ready();
    checks.push(DoctorCheck {
        name: "server.base_url",
        ok: base_ok,
        detail: if base_ok {
            config.base_url.clone()
        } else {
            "base_url empty — set server.base_url or HERMES_SERVER_URL".to_string()
        },
    });

    if !base_ok {
        checks.push(DoctorCheck {
            name: "auth.manager",
            ok: false,
            detail: "cannot initialize API client without base_url".to_string(),
        });
        return DoctorReport { checks };
    }

    match AuthManager::new(config.clone(), &hermes_home) {
        Ok(manager) => {
            match manager.whoami().await {
                Ok(status) => {
                    checks.push(DoctorCheck {
                        name: "auth.token",
                        ok: status.is_logged_in(),
                        detail: if status.is_logged_in() {
                            format!(
                                "logged in via {} {}",
                                status.source,
                                if status.token_expired() {
                                    "(token may be expired)"
                                } else {
                                    ""
                                }
                            )
                        } else {
                            format!("not logged in ({})", status.source)
                        },
                    });

                    if status.is_logged_in() {
                        match manager.fetch_profile().await {
                            Ok(profile) => {
                                checks.push(DoctorCheck {
                                    name: "server.user_me",
                                    ok: true,
                                    detail: format!(
                                        "GET /user/me ok — {} (id={})",
                                        profile.display_name(),
                                        profile.id
                                    ),
                                });
                            }
                            Err(err) => {
                                checks.push(DoctorCheck {
                                    name: "server.user_me",
                                    ok: false,
                                    detail: format!("GET /user/me failed: {err}"),
                                });
                            }
                        }
                    } else {
                        checks.push(DoctorCheck {
                            name: "server.user_me",
                            ok: true,
                            detail: "skipped — not logged in".to_string(),
                        });
                    }
                }
                Err(err) => {
                    checks.push(DoctorCheck {
                        name: "auth.token",
                        ok: false,
                        detail: err.to_string(),
                    });
                }
            }

            checks.push(DoctorCheck {
                name: "server.channel_app",
                ok: true,
                detail: format!(
                    "channel={} app={} wechat_app_id={} wechat_base={}",
                    config.channel,
                    config.app,
                    config.effective_wechat_app_id(),
                    config.effective_wechat_base_url()
                ),
            });

            let stored = config.auth.wechat_app_id.trim();
            if !stored.is_empty()
                && !hermes_config::is_valid_wechat_open_app_id(stored)
            {
                checks.push(DoctorCheck {
                    name: "server.wechat_app_id",
                    ok: false,
                    detail: format!(
                        "stored wechat_app_id '{stored}' is invalid — \
                         login uses channel default {} instead. \
                         Run `hermes server config set channel {}` to fix config.yaml",
                        config.effective_wechat_app_id(),
                        config.channel
                    ),
                });
            } else {
                checks.push(DoctorCheck {
                    name: "server.wechat_app_id",
                    ok: true,
                    detail: config.effective_wechat_app_id(),
                });
            }
        }
        Err(err) => {
            checks.push(DoctorCheck {
                name: "auth.manager",
                ok: false,
                detail: err.to_string(),
            });
        }
    }

    let source = if std::env::var("HERMES_SERVER_TOKEN")
        .ok()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        TokenSource::Environment
    } else {
        TokenSource::None
    };
    checks.push(DoctorCheck {
        name: "auth.token_source",
        ok: true,
        detail: source.to_string(),
    });

    DoctorReport { checks }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::ServerConfig;

    #[tokio::test]
    async fn doctor_without_base_url_reports_missing() {
        let config = ServerConfig::default();
        let report = run_doctor(&config, std::env::temp_dir()).await;
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.name == "server.base_url" && !c.ok)
        );
    }
}
