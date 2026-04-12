//! E2E：真实 `hermes gateway start` 子进程 + 磁盘上的 cron 任务 + `webhooks.json`，
//! 调度器 tick 后由 gateway 侧车向 WireMock 投递 `cron_job_finished` JSON。
//!
//! 依赖 `HERMES_CRON_TICK_SECS`（由子进程设置）缩短 `hermes-cron` 轮询间隔；仅 Unix。

#[cfg(unix)]
mod unix {
    use assert_cmd::cargo::cargo_bin;
    use chrono::{Duration, Utc};
    use hermes_cron::CronJob;
    use serde_json::json;
    use std::fs;
    use std::path::Path;
    use std::process::{Command as SysCommand, Stdio};
    use std::thread;
    use std::time::{Duration as StdDuration, Instant};
    use tokio::time::{sleep, Duration as TokioDuration};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const WEBHOOK_WAIT_SECS: u64 = 45;
    const READY_TIMEOUT: StdDuration = StdDuration::from_secs(45);
    const EXIT_TIMEOUT: StdDuration = StdDuration::from_secs(30);

    fn wait_gateway_pid(pid_path: &Path, child: &mut std::process::Child) -> u32 {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("timeout waiting for {}", pid_path.display());
            }
            if let Ok(Some(status)) = child.try_wait() {
                panic!("gateway exited before ready: {status}");
            }
            if let Ok(raw) = fs::read_to_string(pid_path) {
                if let Ok(pid) = raw.trim().parse::<u32>() {
                    return pid;
                }
            }
            thread::sleep(StdDuration::from_millis(100));
        }
    }

    fn wait_child_exit(
        child: &mut std::process::Child,
        deadline: Instant,
    ) -> std::process::ExitStatus {
        loop {
            if let Ok(Some(s)) = child.try_wait() {
                return s;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("timeout waiting for gateway child to exit");
            }
            thread::sleep(StdDuration::from_millis(100));
        }
    }

    #[test]
    fn gateway_cron_completion_delivers_json_to_wiremock() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .expect("runtime");

        let mock = rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/hook"))
                .respond_with(ResponseTemplate::new(200))
                .expect(1)
                .mount(&server)
                .await;
            server
        });

        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();
        let cron_dir = home.join("cron");
        fs::create_dir_all(&cron_dir).expect("cron dir");

        let mut job = CronJob::new("0 * * * *", "e2e cron webhook probe");
        job.id = "e2e-gateway-webhook-job".to_string();
        job.next_run = Some(Utc::now() - Duration::minutes(5));
        job.repeat = Some(1);

        let job_json =
            serde_json::to_string_pretty(&job).expect("serialize CronJob");
        fs::write(cron_dir.join(format!("{}.json", job.id)), job_json.as_bytes())
            .expect("write job json");

        let hook_url = format!("{}/hook", mock.uri());
        let webhooks = json!({
            "webhooks": [{
                "id": "w-e2e",
                "url": hook_url,
                "created_at": "2020-01-01T00:00:00Z"
            }]
        });
        fs::write(home.join("webhooks.json"), serde_json::to_vec_pretty(&webhooks).unwrap())
            .expect("webhooks.json");

        let mut child = SysCommand::new(cargo_bin("hermes"))
            .env("HERMES_HOME", home)
            .env("HERMES_CRON_TICK_SECS", "1")
            .env_remove("OPENAI_API_KEY")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENROUTER_API_KEY")
            .env_remove("DASHSCOPE_API_KEY")
            .env_remove("MOONSHOT_API_KEY")
            .env_remove("MINIMAX_API_KEY")
            .args(["gateway", "start"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn gateway");

        let pid_path = home.join("gateway.pid");
        let file_pid = wait_gateway_pid(&pid_path, &mut child);

        rt.block_on(async {
            let deadline = Instant::now() + StdDuration::from_secs(WEBHOOK_WAIT_SECS);
            loop {
                if Instant::now() > deadline {
                    panic!("timeout waiting for webhook POST to WireMock");
                }
                let n = mock
                    .received_requests()
                    .await
                    .map(|v| v.len())
                    .unwrap_or(0);
                if n >= 1 {
                    break;
                }
                sleep(TokioDuration::from_millis(150)).await;
            }
        });

        let reqs = rt
            .block_on(mock.received_requests())
            .expect("received_requests");
        assert_eq!(reqs.len(), 1, "expected exactly one webhook POST");
        let body: serde_json::Value =
            serde_json::from_slice(reqs[0].body.as_slice()).expect("webhook JSON body");
        assert_eq!(body["event"], "cron_job_finished");
        assert_eq!(body["job_id"], job.id);
        assert_eq!(body["trigger"], "schedule");
        assert_eq!(body["ok"], false, "no API key → StubProvider fails; webhook still fires");
        assert!(body["error"].is_string(), "expected error string when run fails");

        let kill_st = SysCommand::new("kill")
            .args(["-INT", &file_pid.to_string()])
            .status()
            .expect("kill -INT");
        assert!(kill_st.success(), "kill -INT failed: {kill_st:?}");

        let status = wait_child_exit(&mut child, Instant::now() + EXIT_TIMEOUT);
        assert!(
            status.success(),
            "gateway should exit 0 after SIGINT, got {status:?}"
        );
        assert!(!pid_path.exists(), "gateway.pid should be removed");

        rt.block_on(mock.verify());
    }
}
