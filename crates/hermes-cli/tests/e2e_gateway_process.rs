//! E2E：短生命周期真实 `hermes gateway start` 子进程（无聊天适配器，仅 cron + webhook 侧车）。
//!
//! - 等待 `gateway.pid` 出现即视为就绪；
//! - `hermes gateway status` 应报告 running；
//! - 对子进程发 `SIGINT` 或 `SIGTERM`，均应走同一条优雅清理路径并删除 PID 文件。
//!
//! Windows 上信号语义与 `kill` 不同，测试仅在 Unix 注册。

#[cfg(unix)]
mod unix {
    use assert_cmd::cargo::cargo_bin;
    use assert_cmd::Command;
    use std::fs;
    use std::path::Path;
    use std::process::{Command as SysCommand, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    const READY_TIMEOUT: Duration = Duration::from_secs(60);
    const EXIT_TIMEOUT: Duration = Duration::from_secs(40);

    fn wait_gateway_ready(pid_path: &Path, child: &mut std::process::Child) -> u32 {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!(
                    "timeout waiting for {}; child id {:?}",
                    pid_path.display(),
                    child.id()
                );
            }
            if let Ok(Some(status)) = child.try_wait() {
                panic!(
                    "gateway exited before ready (status={status}); expected {}",
                    pid_path.display()
                );
            }
            if let Ok(raw) = fs::read_to_string(pid_path) {
                if let Ok(pid) = raw.trim().parse::<u32>() {
                    return pid;
                }
            }
            thread::sleep(Duration::from_millis(100));
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
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn e2e_gateway_subprocess_lifecycle_start_status_then_signal(signal: &str) {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();
        let pid_path = home.join("gateway.pid");

        let mut child = SysCommand::new(cargo_bin("hermes"))
            .env("HERMES_HOME", home)
            .env("HOME", home)
            .args(["gateway", "start"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn hermes gateway start");

        let file_pid = wait_gateway_ready(&pid_path, &mut child);
        assert!(file_pid > 0, "gateway.pid should contain a positive PID");

        let mut status = Command::cargo_bin("hermes").expect("hermes binary");
        status
            .env("HERMES_HOME", home)
            .env("HOME", home)
            .args(["gateway", "status"]);
        let out = status.assert().success().get_output().stdout.clone();
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("running"),
            "expected 'running' in gateway status, got: {text:?}"
        );

        let signal_flag = format!("-{signal}");
        let pid_text = file_pid.to_string();
        let kill_st = SysCommand::new("kill")
            .args([signal_flag.as_str(), pid_text.as_str()])
            .status()
            .expect("send shutdown signal");
        assert!(
            kill_st.success(),
            "kill {signal_flag} failed: {kill_st:?}"
        );

        let status = wait_child_exit(&mut child, Instant::now() + EXIT_TIMEOUT);
        assert!(
            status.success(),
            "gateway should exit 0 after {signal} shutdown, got {status:?}"
        );

        assert!(
            !pid_path.exists(),
            "gateway.pid should be removed after graceful {signal} shutdown"
        );
    }

    #[test]
    fn e2e_gateway_subprocess_lifecycle_start_status_sigint() {
        e2e_gateway_subprocess_lifecycle_start_status_then_signal("INT");
    }

    #[test]
    fn e2e_gateway_subprocess_lifecycle_start_status_sigterm() {
        e2e_gateway_subprocess_lifecycle_start_status_then_signal("TERM");
    }

    #[test]
    fn e2e_gateway_stop_command_waits_for_graceful_exit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();
        let pid_path = home.join("gateway.pid");

        let mut child = SysCommand::new(cargo_bin("hermes"))
            .env("HERMES_HOME", home)
            .env("HOME", home)
            .args(["gateway", "start"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn hermes gateway start");

        let file_pid = wait_gateway_ready(&pid_path, &mut child);
        let mut stop = Command::cargo_bin("hermes").expect("hermes binary");
        stop.env("HERMES_HOME", home)
            .env("HOME", home)
            .args(["gateway", "stop"]);
        let out = stop.assert().success().get_output().stdout.clone();
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("Gateway stopped."),
            "expected a completed graceful stop, got: {text:?}"
        );

        let status = wait_child_exit(&mut child, Instant::now() + EXIT_TIMEOUT);
        assert!(
            status.success(),
            "gateway should exit 0 after stop command, got {status:?}"
        );
        assert!(
            !pid_path.exists(),
            "gateway.pid should be removed only after the stop command observes exit for PID {file_pid}"
        );
    }

    #[test]
    fn e2e_gateway_start_refuses_live_gateway_metadata() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();
        let pid_path = home.join("gateway.pid");
        let lock_path = home.join("gateway.lock");
        let mut child = SysCommand::new("sh")
            .args([
                "-c",
                "trap '' TERM; while :; do sleep 1; done",
                "hermes-gateway",
            ])
            .spawn()
            .expect("spawn uncooperative gateway-shaped process");
        let pid = child.id();
        std::thread::sleep(Duration::from_millis(50));
        fs::write(&pid_path, format!("{pid}\n")).expect("write gateway PID");
        fs::write(&lock_path, format!(r#"{{"pid":{pid},"kind":"hermes-gateway"}}"#))
            .expect("write gateway lock");

        let output = SysCommand::new(cargo_bin("hermes"))
            .env("HERMES_HOME", home)
            .env("HOME", home)
            .args(["gateway", "start"])
            .output()
            .expect("run gateway start");
        let pid_retained = pid_path.exists();
        let lock_retained = lock_path.exists();
        let _ = child.kill();
        let _ = child.wait();

        assert!(
            !output.status.success(),
            "start must refuse to overlap a live gateway: {output:?}"
        );
        assert!(pid_retained, "start must not overwrite live PID metadata");
        assert!(lock_retained, "start must not overwrite live lock metadata");
    }
}
