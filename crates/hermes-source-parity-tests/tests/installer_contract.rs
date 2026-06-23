use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct TempTree {
    path: PathBuf,
}

impl TempTree {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "hermes-installer-contract-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp tree");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct InstallerFixture {
    _temp: TempTree,
    root: PathBuf,
    install_dir: PathBuf,
    home_dir: PathBuf,
    tools_dir: PathBuf,
    invocation_log: PathBuf,
}

impl InstallerFixture {
    fn new(label: &str) -> Self {
        let temp = TempTree::new(label);
        let root = temp.path().to_path_buf();
        let install_dir = root.join("bin");
        let home_dir = root.join("home");
        let tools_dir = root.join("tools");
        let invocation_log = root.join("hermes-invocations.log");

        fs::create_dir_all(&install_dir).expect("create install dir");
        fs::create_dir_all(&home_dir).expect("create home dir");
        fs::create_dir_all(&tools_dir).expect("create fake tools dir");

        write_executable(
            &tools_dir.join("curl"),
            r#"#!/usr/bin/env sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    out="$2"
    shift 2
  else
    shift
  fi
done
if [ -z "$out" ]; then
  exit 2
fi
mkdir -p "$(dirname "$out")"
: > "$out"
"#,
        )
        .expect("write fake curl");

        write_executable(
            &tools_dir.join("tar"),
            r#"#!/usr/bin/env sh
outdir=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-C" ]; then
    outdir="$2"
    shift 2
  else
    shift
  fi
done
if [ -z "$outdir" ]; then
  exit 2
fi
mkdir -p "$outdir"
cat > "$outdir/hermes-agent-ultra" <<'BIN'
#!/usr/bin/env sh
printf '%s\n' "$*" >> "${HERMES_FAKE_BIN_LOG:?}"
exit 0
BIN
chmod +x "$outdir/hermes-agent-ultra"
"#,
        )
        .expect("write fake tar");

        Self {
            _temp: temp,
            root,
            install_dir,
            home_dir,
            tools_dir,
            invocation_log,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new("bash");
        command
            .arg(repo_root().join("scripts/install.sh"))
            .arg("--version")
            .arg("v-test")
            .arg("--dir")
            .arg(&self.install_dir)
            .env("HOME", &self.home_dir)
            .env("HERMES_HOME", self.root.join("hermes-home"))
            .env("HERMES_FAKE_BIN_LOG", &self.invocation_log)
            .env("HERMES_INSTALL_PROBE_TIMEOUT_SECONDS", "3")
            .env("PATH", fixture_path(&self.tools_dir))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run(&self, extra_args: &[&str]) -> Output {
        let mut command = self.command();
        for arg in extra_args {
            command.arg(arg);
        }
        command.output().expect("run installer")
    }
}

fn fixture_path(tools_dir: &Path) -> String {
    let existing = std::env::var_os("PATH").unwrap_or_default();
    format!("{}:{}", tools_dir.display(), existing.to_string_lossy())
}

fn write_executable(path: &Path, content: &str) -> io::Result<()> {
    fs::write(path, content)?;
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn output_text(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn noninteractive_auto_install_skips_post_install_verification() {
    let fixture = InstallerFixture::new("noninteractive-auto");

    let output = fixture.run(&[]);
    let text = output_text(&output);

    assert!(output.status.success(), "{text}");
    assert!(
        text.contains("Post-install setup skipped (non-interactive install). Run later:"),
        "{text}"
    );
    assert!(
        text.contains(
            "To run verification during install, pass --setup or set RUN_SETUP_MODE=always."
        ),
        "{text}"
    );
    assert!(
        !text.contains("Running post-install verification"),
        "{text}"
    );
    assert!(
        !fixture.invocation_log.exists(),
        "installed binary should not be invoked in non-interactive auto mode"
    );
}

#[test]
fn explicit_setup_runs_bounded_post_install_probes() {
    let fixture = InstallerFixture::new("explicit-setup");

    let started = Instant::now();
    let output = fixture.run(&["--setup"]);
    let elapsed = started.elapsed();
    let text = output_text(&output);

    assert!(output.status.success(), "{text}");
    assert!(
        elapsed < Duration::from_secs(5),
        "fast probes must not wait for the full watchdog timeout; elapsed={elapsed:?}\n{text}"
    );
    assert!(text.contains("Running post-install verification"), "{text}");
    assert!(text.contains("Current auth/platform status:"), "{text}");

    let invocations =
        fs::read_to_string(&fixture.invocation_log).expect("installed binary should be invoked");
    assert!(
        invocations.lines().any(|line| line == "doctor"),
        "{invocations}"
    );
    assert!(
        invocations.lines().any(|line| line == "auth status"),
        "{invocations}"
    );
    assert!(
        !invocations.lines().any(|line| line == "setup"),
        "non-TTY explicit setup should run probes but skip interactive setup: {invocations}"
    );
}

#[test]
fn default_install_preserves_existing_upstream_hermes_command() {
    let fixture = InstallerFixture::new("coexist-upstream");
    let upstream_hermes = fixture.install_dir.join("hermes");
    write_executable(
        &upstream_hermes,
        "#!/usr/bin/env sh\necho upstream hermes\n",
    )
    .expect("seed upstream hermes");

    let output = fixture.run(&[]);
    let text = output_text(&output);

    assert!(output.status.success(), "{text}");
    assert!(
        text.contains("Legacy hermes alias not installed by default; existing upstream hermes commands are left untouched."),
        "{text}"
    );
    assert!(fixture.install_dir.join("hermes-agent-ultra").is_file());
    assert_eq!(
        fs::read_link(fixture.install_dir.join("hermes-ultra")).expect("primary symlink"),
        PathBuf::from("hermes-agent-ultra")
    );
    assert!(
        fs::symlink_metadata(&upstream_hermes)
            .expect("upstream hermes remains")
            .file_type()
            .is_file(),
        "default installer must not replace upstream hermes with a symlink"
    );
    assert_eq!(
        fs::read_to_string(&upstream_hermes).expect("read upstream hermes"),
        "#!/usr/bin/env sh\necho upstream hermes\n"
    );
}
