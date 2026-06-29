use std::time::Duration;

use super::DEFAULT_HERMES_HTTP_PORT;

#[derive(Debug, Clone)]
pub struct HttpProbeResult {
    pub base_url: String,
    pub version: Option<String>,
    pub ok: bool,
}

pub async fn probe_status(base_url: Option<&str>) -> HttpProbeResult {
    let base = base_url
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| format!("http://127.0.0.1:{DEFAULT_HERMES_HTTP_PORT}"));

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            return HttpProbeResult {
                base_url: base,
                version: None,
                ok: false,
            };
        }
    };

    let url = format!("{base}/api/status");
    match client.get(url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let version = resp.json::<serde_json::Value>().await.ok().and_then(|v| {
                v.get("version")
                    .and_then(|x| x.as_str())
                    .map(str::to_string)
            });
            HttpProbeResult {
                base_url: base,
                version,
                ok: true,
            }
        }
        _ => HttpProbeResult {
            base_url: base,
            version: None,
            ok: false,
        },
    }
}

pub fn probe_status_blocking(base_url: Option<&str>) -> HttpProbeResult {
    let base = base_url
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| format!("http://127.0.0.1:{DEFAULT_HERMES_HTTP_PORT}"));

    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            return HttpProbeResult {
                base_url: base,
                version: None,
                ok: false,
            };
        }
    };

    let url = format!("{base}/api/status");
    match client.get(url).send() {
        Ok(resp) if resp.status().is_success() => {
            let version = resp.json::<serde_json::Value>().ok().and_then(|v| {
                v.get("version")
                    .and_then(|x| x.as_str())
                    .map(str::to_string)
            });
            HttpProbeResult {
                base_url: base,
                version,
                ok: true,
            }
        }
        _ => HttpProbeResult {
            base_url: base,
            version: None,
            ok: false,
        },
    }
}
