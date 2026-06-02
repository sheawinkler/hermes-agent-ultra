use crate::update::github::{GitHubSource, ReleaseSource};
use crate::update::modelscope::ModelScopeSource;

/// 选择最快的更新源
///
/// 如果 source_override 指定了 "github" 或 "modelscope"，直接返回对应源。
/// 否则检查 HERMES_UPDATE_SOURCE 环境变量。
/// 都没有则并发探测延迟，选择最快响应的。
pub async fn select_fastest_source(source_override: Option<&str>) -> Box<dyn ReleaseSource> {
    // 1. 检查强制指定
    let forced = source_override
        .map(|s| s.to_string())
        .or_else(|| std::env::var("HERMES_UPDATE_SOURCE").ok());

    if let Some(ref src) = forced {
        match src.as_str() {
            "github" => return Box::new(GitHubSource::new()),
            "modelscope" => return Box::new(ModelScopeSource::new()),
            _ => tracing::warn!("Unknown source '{}', falling back to auto-detect", src),
        }
    }

    // 2. 并发探测（与 github.rs / modelscope.rs 保持一致，使用系统 curl）
    let github = probe_url("https://api.github.com");
    let modelscope = probe_url("https://modelscope.cn");

    tokio::pin!(github);
    tokio::pin!(modelscope);

    let mut github_first = false;

    // tokio::select! 取第一个完成的
    let first_result: Option<()> = tokio::select! {
        res = &mut github => {
            github_first = true;
            if res {
                tracing::info!("Auto-detected fastest source: GitHub");
                return Box::new(GitHubSource::new());
            }
            // GitHub 探测失败，等 ModelScope
            None
        }
        res = &mut modelscope => {
            if res {
                tracing::info!("Auto-detected fastest source: ModelScope");
                return Box::new(ModelScopeSource::new());
            }
            // ModelScope 探测失败，等 GitHub
            None
        }
    };

    // 第一个完成的失败了，等另一个
    if first_result.is_none() {
        let second_ok = if !github_first {
            // ModelScope 先完成但失败了，等 GitHub
            github.await
        } else {
            // GitHub 先完成但失败了，等 ModelScope
            modelscope.await
        };

        if second_ok {
            if !github_first {
                tracing::info!("Auto-detected fastest source: GitHub");
                return Box::new(GitHubSource::new());
            } else {
                tracing::info!("Auto-detected fastest source: ModelScope");
                return Box::new(ModelScopeSource::new());
            }
        }
    }

    // 都失败则默认 GitHub
    tracing::warn!("All source probes failed, defaulting to GitHub");
    Box::new(GitHubSource::new())
}

/// 使用系统 curl 发送 HEAD 请求探测 URL 可达性。
/// curl HEAD 是阻塞的，通过 `tokio::spawn_blocking` 包装。
/// 返回 true 表示 curl 成功退出（exit code 0），即网络可达。
async fn probe_url(url: &str) -> bool {
    let url = url.to_string();
    tokio::task::spawn_blocking(move || {
        let status = std::process::Command::new("curl")
            .args(["-sSI", "--max-time", "3", &url])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        match status {
            Ok(s) => {
                if !s.success() {
                    tracing::debug!("Probe {} failed: curl exit code {:?}", url, s.code());
                }
                s.success()
            }
            Err(e) => {
                tracing::debug!("Probe {} failed to launch curl: {}", url, e);
                false
            }
        }
    })
    .await
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_forced_source_github() {
        let source = select_fastest_source(Some("github")).await;
        assert_eq!(source.name(), "GitHub");
    }

    #[tokio::test]
    async fn test_forced_source_modelscope() {
        let source = select_fastest_source(Some("modelscope")).await;
        assert_eq!(source.name(), "ModelScope");
    }

    #[tokio::test]
    async fn test_env_var_override() {
        // SAFETY: test-only env manipulation; tests run single-threaded for env vars
        unsafe { std::env::set_var("HERMES_UPDATE_SOURCE", "modelscope") };
        let source = select_fastest_source(None).await;
        assert_eq!(source.name(), "ModelScope");
        // Cleanup
        unsafe { std::env::remove_var("HERMES_UPDATE_SOURCE") };
    }
}
