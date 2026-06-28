#[cfg(unix)]
fn configure_foreground_process_group(cmd: &mut TokioCommand) {
    use std::os::unix::process::CommandExt;

    cmd.as_std_mut().process_group(0);
}

#[cfg(not(unix))]
fn configure_foreground_process_group(_cmd: &mut TokioCommand) {}

struct ForegroundChildGuard {
    child: Option<tokio::process::Child>,
    pid: Option<u32>,
}

impl ForegroundChildGuard {
    fn new(child: tokio::process::Child) -> Self {
        let pid = child.id();
        Self {
            child: Some(child),
            pid,
        }
    }

    fn child_mut(&mut self) -> &mut tokio::process::Child {
        self.child.as_mut().expect("foreground child present")
    }

    fn disarm(&mut self) {
        self.child.take();
    }
}

impl Drop for ForegroundChildGuard {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        terminate_child_process_sync(self.pid);
        let _ = child.start_kill();
    }
}

#[cfg(unix)]
fn terminate_child_process_sync(pid: Option<u32>) {
    if let Some(pid) = pid {
        let pgid = -(pid as i32);
        unsafe {
            libc::kill(pgid, libc::SIGTERM);
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
        unsafe {
            libc::kill(pgid, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn terminate_child_process_sync(_pid: Option<u32>) {}

#[cfg(unix)]
async fn terminate_child_process(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        let pgid = -(pid as i32);
        unsafe {
            libc::kill(pgid, libc::SIGTERM);
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) | Err(_) => unsafe {
                libc::kill(pgid, libc::SIGKILL);
            },
        }
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[cfg(not(unix))]
async fn terminate_child_process(child: &mut tokio::process::Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn hermes_home_dir() -> Option<PathBuf> {
    std::env::var_os("HERMES_HOME")
        .or_else(|| std::env::var_os("HERMES_AGENT_ULTRA_HOME"))
        .map(PathBuf::from)
}

fn current_username() -> Option<String> {
    std::env::var("USER")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("LOGNAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| {
            std::env::var("USERNAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
}

fn is_valid_unix_username(username: &str) -> bool {
    !username.is_empty()
        && username
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

#[cfg(unix)]
fn passwd_home_for_username(username: &str) -> Option<PathBuf> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split(':');
        let user = parts.next()?;
        let _passwd = parts.next()?;
        let _uid = parts.next()?;
        let _gid = parts.next()?;
        let _gecos = parts.next()?;
        let home = parts.next()?;
        if user == username && !home.is_empty() {
            return Some(PathBuf::from(home));
        }
    }
    None
}

#[cfg(unix)]
fn lookup_home_for_username(username: &str) -> Option<PathBuf> {
    if current_username().as_deref() == Some(username) {
        return home_dir();
    }
    passwd_home_for_username(username)
}

#[cfg(not(unix))]
fn lookup_home_for_username(username: &str) -> Option<PathBuf> {
    if current_username().as_deref() == Some(username) {
        return home_dir();
    }
    None
}

fn real_home_dir() -> Option<PathBuf> {
    std::env::var_os("HERMES_REAL_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            #[cfg(unix)]
            {
                current_username()
                    .as_deref()
                    .and_then(passwd_home_for_username)
            }
            #[cfg(not(unix))]
            {
                None
            }
        })
        .or_else(home_dir)
}

fn ensure_profile_home_dir() -> Option<PathBuf> {
    let home = hermes_home_dir()?.join("home");
    if std::fs::create_dir_all(&home).is_ok() {
        Some(home)
    } else {
        None
    }
}

fn subprocess_home_for_mode(mode: TerminalHomeMode) -> Option<PathBuf> {
    match mode {
        TerminalHomeMode::Auto | TerminalHomeMode::Real => real_home_dir(),
        TerminalHomeMode::Profile => ensure_profile_home_dir().or_else(real_home_dir),
    }
}

fn resolve_path(input: &str) -> Result<PathBuf, AgentError> {
    if !input.starts_with('~') {
        let path = PathBuf::from(input);
        if path.is_absolute() {
            return Ok(path);
        }
        if let Some(cwd) = std::env::var_os("TERMINAL_CWD") {
            if !cwd.is_empty() {
                return Ok(PathBuf::from(cwd).join(path));
            }
        }
        return Ok(path);
    }

    let rest = &input[1..];
    if rest.is_empty() {
        return home_dir().ok_or_else(|| AgentError::Io("Failed to resolve home dir".into()));
    }

    if rest.starts_with('/') {
        let home = home_dir().ok_or_else(|| AgentError::Io("Failed to resolve home dir".into()))?;
        let suffix = rest.trim_start_matches('/');
        return Ok(if suffix.is_empty() {
            home
        } else {
            home.join(suffix)
        });
    }

    // ~username or ~username/path. For security, only allow traditional
    // username characters so malicious payloads cannot pass through.
    let (username, suffix) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx + 1..]),
        None => (rest, ""),
    };

    if !is_valid_unix_username(username) {
        return Ok(PathBuf::from(input));
    }

    if let Some(home) = lookup_home_for_username(username) {
        return Ok(if suffix.is_empty() {
            home
        } else {
            home.join(suffix)
        });
    }

    Ok(PathBuf::from(input))
}

const SUBPROCESS_ENV_BLOCKLIST_EXACT: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
    "BROWSERBASE_PROJECT_ID",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "COHERE_API_KEY",
    "DAYTONA_API_KEY",
    "DEEPSEEK_API_KEY",
    "DISCORD_FREE_RESPONSE_CHANNELS",
    "DISCORD_HOME_CHANNEL",
    "DISCORD_HOME_CHANNEL_NAME",
    "DISCORD_REQUIRE_MENTION",
    "EMAIL_ADDRESS",
    "EMAIL_HOME_ADDRESS",
    "EMAIL_HOME_ADDRESS_NAME",
    "EMAIL_IMAP_HOST",
    "EMAIL_PASSWORD",
    "EMAIL_SMTP_HOST",
    "ELEVENLABS_API_KEY",
    "FIRECRAWL_API_KEY",
    "FIREWORKS_API_KEY",
    "GATEWAY_ALLOW_ALL_USERS",
    "GATEWAY_ALLOWED_USERS",
    "GH_TOKEN",
    "GITHUB_APP_ID",
    "GITHUB_APP_INSTALLATION_ID",
    "GITHUB_APP_PRIVATE_KEY_PATH",
    "GITHUB_TOKEN",
    "GLM_API_KEY",
    "GOOGLE_API_KEY",
    "GROQ_API_KEY",
    "HASS_TOKEN",
    "HASS_URL",
    "HELICONE_API_KEY",
    "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
    "HERMES_POLICY_ADMIN_TOKEN",
    "KIMI_API_KEY",
    "LLM_MODEL",
    "MINIMAX_API_KEY",
    "MINIMAX_CN_API_KEY",
    "MISTRAL_API_KEY",
    "MODAL_TOKEN_ID",
    "MODAL_TOKEN_SECRET",
    "NVIDIA_API_KEY",
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "OPENROUTER_API_KEY",
    "PERPLEXITY_API_KEY",
    "SIGNAL_ACCOUNT",
    "SIGNAL_ALLOWED_USERS",
    "SIGNAL_GROUP_ALLOWED_USERS",
    "SIGNAL_HOME_CHANNEL",
    "SIGNAL_HOME_CHANNEL_NAME",
    "SIGNAL_HTTP_URL",
    "SIGNAL_IGNORE_STORIES",
    "SLACK_ALLOWED_USERS",
    "SLACK_APP_TOKEN",
    "SLACK_HOME_CHANNEL",
    "SLACK_HOME_CHANNEL_NAME",
    "TELEGRAM_BOT_TOKEN",
    "TELEGRAM_HOME_CHANNEL",
    "TELEGRAM_HOME_CHANNEL_NAME",
    "TOGETHER_API_KEY",
    "WHATSAPP_ALLOWED_USERS",
    "WHATSAPP_ENABLED",
    "WHATSAPP_MODE",
    "XAI_API_KEY",
    "ZAI_API_KEY",
    "Z_AI_API_KEY",
];

const SUBPROCESS_ENV_BLOCKLIST_PREFIXES: &[&str] = &[
    "TOOL_GATEWAY_",
    "HERMES_MANAGED_TOOL_GATEWAY_",
    "HERMES_GATEWAY_",
    "HERMES_HTTP_",
];

const SUBPROCESS_ENV_FORCE_PREFIX: &str = "_HERMES_FORCE_";
const SUBPROCESS_ENV_PASSTHROUGH_VAR: &str = "HERMES_SUBPROCESS_ENV_PASSTHROUGH";

const SANE_PATH_ENTRIES: &[&str] = &[
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
];

fn should_strip_subprocess_env(key: &str) -> bool {
    SUBPROCESS_ENV_BLOCKLIST_EXACT.contains(&key)
        || SUBPROCESS_ENV_BLOCKLIST_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
}

fn normalize_env_passthrough_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_subprocess_env_passthrough(value: &str) -> BTreeSet<String> {
    value
        .split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ':' | ';'))
        .filter_map(normalize_env_passthrough_name)
        .collect()
}

fn subprocess_env_passthrough_set(configured: &[String]) -> BTreeSet<String> {
    let mut values = std::env::var(SUBPROCESS_ENV_PASSTHROUGH_VAR)
        .ok()
        .map(|raw| parse_subprocess_env_passthrough(&raw))
        .unwrap_or_default();
    values.extend(
        configured
            .iter()
            .filter_map(|name| normalize_env_passthrough_name(name)),
    );
    values
}

fn is_subprocess_env_passthrough(key: &str, passthrough: &BTreeSet<String>) -> bool {
    passthrough.contains(key)
}

fn normalize_subprocess_path(path: Option<&str>) -> String {
    let Some(path) = path.filter(|value| !value.trim().is_empty()) else {
        return SANE_PATH_ENTRIES.join(":");
    };
    if std::env::split_paths(path).any(|entry| entry == std::path::Path::new("/usr/bin")) {
        return path.to_string();
    }

    let mut entries: Vec<String> = std::env::split_paths(path)
        .map(|entry| entry.to_string_lossy().to_string())
        .filter(|entry| !entry.is_empty())
        .collect();
    for sane in SANE_PATH_ENTRIES {
        if !entries.iter().any(|entry| entry == sane) {
            entries.push((*sane).to_string());
        }
    }
    entries.join(":")
}

fn shell_env_cleanup_snippet(configured_passthrough: &[String]) -> String {
    let mut snippet = String::new();
    let configured_passthrough = configured_passthrough
        .iter()
        .filter_map(|name| normalize_env_passthrough_name(name))
        .collect::<Vec<_>>()
        .join(" ");
    if !configured_passthrough.is_empty() {
        snippet.push_str(
            "HERMES_SUBPROCESS_ENV_PASSTHROUGH=\"${HERMES_SUBPROCESS_ENV_PASSTHROUGH:-} ",
        );
        snippet.push_str(&configured_passthrough);
        snippet.push_str("\"; ");
    }
    if !SUBPROCESS_ENV_BLOCKLIST_PREFIXES.is_empty() {
        snippet
            .push_str("for __hermes_env in $(env | sed 's/=.*//'); do case \"$__hermes_env\" in ");
        for (idx, prefix) in SUBPROCESS_ENV_BLOCKLIST_PREFIXES.iter().enumerate() {
            if idx > 0 {
                snippet.push('|');
            }
            snippet.push_str(prefix);
            snippet.push('*');
        }
        snippet.push_str(") case \" ${HERMES_SUBPROCESS_FORCE_TARGETS:-} ${HERMES_SUBPROCESS_ENV_PASSTHROUGH:-} \" in *\" $__hermes_env \"*) ;; *) unset \"$__hermes_env\" ;; esac ;; esac; done; unset __hermes_env; ");
    }
    for key in SUBPROCESS_ENV_BLOCKLIST_EXACT {
        snippet.push_str("case \" ${HERMES_SUBPROCESS_FORCE_TARGETS:-} ${HERMES_SUBPROCESS_ENV_PASSTHROUGH:-} \" in *\" ");
        snippet.push_str(key);
        snippet.push_str(" \"*) ;; *) unset ");
        snippet.push_str(key);
        snippet.push_str(" ;; esac; ");
    }
    snippet.push_str("unset HERMES_SUBPROCESS_FORCE_TARGETS; ");
    snippet
}

fn parse_shell_init_list(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.starts_with('[') {
        if let Ok(values) = serde_json::from_str::<Vec<String>>(trimmed) {
            return values
                .into_iter()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .collect();
        }
    }
    let delimiter = if trimmed.contains(',') { ',' } else { ':' };
    trimmed
        .split(delimiter)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn bool_env_or_default(name: &str, default: bool) -> bool {
    let Some(value) = std::env::var(name).ok() else {
        return default;
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default,
    }
}

fn terminal_config_from_env() -> (Vec<String>, bool, TerminalHomeMode, Vec<String>) {
    let explicit = std::env::var("TERMINAL_SHELL_INIT_FILES")
        .ok()
        .map(|v| parse_shell_init_list(&v))
        .unwrap_or_default();
    let auto_source_bashrc = bool_env_or_default("TERMINAL_AUTO_SOURCE_BASHRC", true);
    let home_mode = std::env::var("TERMINAL_HOME_MODE")
        .ok()
        .and_then(|v| TerminalHomeMode::from_env_name(&v))
        .unwrap_or_default();
    let env_passthrough = std::env::var(SUBPROCESS_ENV_PASSTHROUGH_VAR)
        .ok()
        .map(|v| parse_subprocess_env_passthrough(&v).into_iter().collect())
        .unwrap_or_default();
    (explicit, auto_source_bashrc, home_mode, env_passthrough)
}

fn expand_env_refs(input: &str) -> String {
    let mut output = String::new();
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find('}') else {
            output.push_str(&rest[start..]);
            return output;
        };
        let name = &after_open[..end];
        if !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            if let Ok(value) = std::env::var(name) {
                output.push_str(&value);
            }
        } else {
            output.push_str("${");
            output.push_str(name);
            output.push('}');
        }
        rest = &after_open[end + 1..];
    }
    output.push_str(rest);
    output
}

fn shell_home_dir(home_override: Option<&std::path::Path>) -> Option<PathBuf> {
    home_override.map(PathBuf::from).or_else(home_dir)
}

fn expand_shell_init_path(input: &str, home_override: Option<&std::path::Path>) -> PathBuf {
    let expanded = expand_env_refs(input.trim());
    if expanded == "~" {
        return shell_home_dir(home_override).unwrap_or_else(|| PathBuf::from(expanded));
    }
    if let Some(rest) = expanded.strip_prefix("~/") {
        if let Some(home) = shell_home_dir(home_override) {
            return home.join(rest);
        }
    }
    PathBuf::from(expanded)
}

fn resolve_existing_shell_init_files(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    paths.into_iter().filter(|p| p.is_file()).collect()
}

fn auto_shell_init_candidates(
    shell: &str,
    home_override: Option<&std::path::Path>,
) -> Vec<PathBuf> {
    let Some(home) = shell_home_dir(home_override) else {
        return Vec::new();
    };
    match shell {
        "zsh" => [".zshenv", ".zprofile", ".zshrc", ".profile"]
            .into_iter()
            .map(|file| home.join(file))
            .collect(),
        _ => [".profile", ".bash_profile", ".bashrc"]
            .into_iter()
            .map(|file| home.join(file))
            .collect(),
    }
}

fn resolve_shell_init_files_for_shell(
    shell: &str,
    explicit_files: &[String],
    auto_source_bashrc: bool,
    home_override: Option<&std::path::Path>,
) -> Vec<PathBuf> {
    if !explicit_files.is_empty() {
        return resolve_existing_shell_init_files(
            explicit_files
                .iter()
                .map(|path| expand_shell_init_path(path.as_str(), home_override)),
        );
    }
    if !auto_source_bashrc {
        return Vec::new();
    }
    resolve_existing_shell_init_files(auto_shell_init_candidates(shell, home_override))
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn shell_source_prelude(files: &[PathBuf]) -> String {
    let mut prelude = String::new();
    if files.is_empty() {
        return prelude;
    }
    prelude.push_str("set +e; ");
    for file in files {
        let quoted = shell_single_quote(&file.to_string_lossy());
        prelude.push_str("[ -r ");
        prelude.push_str(&quoted);
        prelude.push_str(" ] && . ");
        prelude.push_str(&quoted);
        prelude.push_str(" || true; ");
    }
    prelude
}

fn scrub_subprocess_env(cmd: &mut TokioCommand, configured_passthrough: &[String]) {
    let passthrough = subprocess_env_passthrough_set(configured_passthrough);
    let mut forced = Vec::new();
    for (key, _) in std::env::vars() {
        if should_strip_subprocess_env(&key) && !is_subprocess_env_passthrough(&key, &passthrough) {
            cmd.env_remove(key);
        } else if let Some(target) = key.strip_prefix(SUBPROCESS_ENV_FORCE_PREFIX) {
            cmd.env_remove(&key);
            if !target.is_empty() && should_strip_subprocess_env(target) {
                if let Ok(value) = std::env::var(&key) {
                    forced.push((target.to_string(), value));
                }
            }
        }
    }
    for (target, value) in forced {
        cmd.env(target, value);
    }
    let forced_targets: Vec<String> = std::env::vars()
        .filter_map(|(key, _)| {
            key.strip_prefix(SUBPROCESS_ENV_FORCE_PREFIX)
                .filter(|target| !target.is_empty() && should_strip_subprocess_env(target))
                .map(ToString::to_string)
        })
        .collect();
    if forced_targets.is_empty() {
        cmd.env_remove("HERMES_SUBPROCESS_FORCE_TARGETS");
    } else {
        cmd.env("HERMES_SUBPROCESS_FORCE_TARGETS", forced_targets.join(" "));
    }
    if passthrough.is_empty() {
        cmd.env_remove(SUBPROCESS_ENV_PASSTHROUGH_VAR);
    } else {
        cmd.env(
            SUBPROCESS_ENV_PASSTHROUGH_VAR,
            passthrough.into_iter().collect::<Vec<_>>().join(" "),
        );
    }

    let normalized_path = normalize_subprocess_path(std::env::var("PATH").ok().as_deref());
    cmd.env("PATH", normalized_path);
}

fn apply_subprocess_home_policy(cmd: &mut TokioCommand, subprocess_home: Option<&PathBuf>) {
    if let Some(real_home) = real_home_dir() {
        cmd.env("HERMES_REAL_HOME", real_home);
    }
    if let Some(home) = subprocess_home {
        cmd.env("HOME", home);
    }
}

fn with_login_profile_sources(
    command: &str,
    explicit_files: &[String],
    auto_source_bashrc: bool,
    env_passthrough: &[String],
    subprocess_home: Option<&std::path::Path>,
) -> String {
    #[cfg(unix)]
    {
        let cleanup = shell_env_cleanup_snippet(env_passthrough);
        let bash_prelude = shell_source_prelude(&resolve_shell_init_files_for_shell(
            "bash",
            explicit_files,
            auto_source_bashrc,
            subprocess_home,
        ));
        let zsh_prelude = shell_source_prelude(&resolve_shell_init_files_for_shell(
            "zsh",
            explicit_files,
            auto_source_bashrc,
            subprocess_home,
        ));
        let bash_command = shell_single_quote(&format!("{bash_prelude}{cleanup}{command}"));
        let zsh_command = shell_single_quote(&format!("{zsh_prelude}{cleanup}{command}"));
        let preferred_shell = std::env::var("SHELL")
            .ok()
            .and_then(|raw| {
                std::path::Path::new(raw.trim())
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.to_ascii_lowercase())
            })
            .filter(|name| matches!(name.as_str(), "bash" | "zsh"));
        let preferred_branch = match preferred_shell.as_deref() {
            Some("zsh") => {
                format!("if command -v zsh >/dev/null 2>&1; then exec zsh -lc {zsh_command}; fi; ")
            }
            Some("bash") => format!(
                "if command -v bash >/dev/null 2>&1; then exec bash -lc {bash_command}; fi; "
            ),
            _ => String::new(),
        };
        format!(
            "{preferred_branch}if command -v bash >/dev/null 2>&1; then exec bash -lc {bash_command}; \
elif command -v zsh >/dev/null 2>&1; then exec zsh -lc {zsh_command}; \
else printf '%s\n' \"Hermes could not find bash or zsh in PATH. Run 'exec zsh -l' or set your default shell where env vars are available.\" >&2; exit 127; fi"
        )
    }
    #[cfg(not(unix))]
    {
        let _ = explicit_files;
        let _ = auto_source_bashrc;
        let _ = env_passthrough;
        let _ = subprocess_home;
        command.to_string()
    }
}

fn rewrite_compound_background(command: &str) -> String {
    let mut out = String::with_capacity(command.len());
    for line in command.split_inclusive('\n') {
        let (body, newline) = line
            .strip_suffix('\n')
            .map(|body| (body, "\n"))
            .unwrap_or((line, ""));
        out.push_str(&rewrite_compound_background_line(body));
        out.push_str(newline);
    }
    out
}

fn rewrite_compound_background_line(line: &str) -> String {
    if line.trim_start().starts_with('#') {
        return line.to_string();
    }
    let Some(amp_idx) = trailing_background_ampersand(line) else {
        return line.to_string();
    };
    let Some(op) = last_top_level_chain_operator(line, amp_idx) else {
        return line.to_string();
    };

    let mut tail_start = op.end;
    while tail_start < amp_idx {
        let Some(ch) = line[tail_start..amp_idx].chars().next() else {
            break;
        };
        if !ch.is_whitespace() {
            break;
        }
        tail_start += ch.len_utf8();
    }
    if line[tail_start..amp_idx].trim().is_empty() {
        return line.to_string();
    }

    let mut rewritten = String::with_capacity(line.len() + 4);
    rewritten.push_str(&line[..tail_start]);
    rewritten.push_str("{ ");
    rewritten.push_str(&line[tail_start..=amp_idx]);
    rewritten.push_str(" }");
    rewritten.push_str(&line[amp_idx + 1..]);
    rewritten
}

#[derive(Clone, Copy)]
struct ChainOperator {
    end: usize,
}

fn trailing_background_ampersand(line: &str) -> Option<usize> {
    let mut idx = line.len();
    while idx > 0 {
        let (prev, ch) = line[..idx].char_indices().next_back()?;
        if !ch.is_whitespace() {
            idx = prev;
            break;
        }
        idx = prev;
    }
    if line[idx..].chars().next()? != '&' || is_escaped(line, idx) {
        return None;
    }
    if idx > 0 && line[..idx].ends_with('&') {
        return None;
    }
    Some(idx)
}

fn is_escaped(input: &str, idx: usize) -> bool {
    let mut count = 0usize;
    let mut pos = idx;
    while pos > 0 {
        let Some((prev, ch)) = input[..pos].char_indices().next_back() else {
            break;
        };
        if ch != '\\' {
            break;
        }
        count += 1;
        pos = prev;
    }
    count % 2 == 1
}

fn last_top_level_chain_operator(line: &str, stop: usize) -> Option<ChainOperator> {
    let mut last = None;
    let mut single = false;
    let mut double = false;
    let mut paren_depth = 0usize;
    let mut command_sub_depth = 0usize;
    let mut iter = line[..stop].char_indices().peekable();

    while let Some((idx, ch)) = iter.next() {
        if is_escaped(line, idx) {
            continue;
        }
        if single {
            if ch == '\'' {
                single = false;
            }
            continue;
        }
        if double {
            if ch == '"' {
                double = false;
                continue;
            }
            if ch == '$' && iter.peek().is_some_and(|(_, next)| *next == '(') {
                command_sub_depth += 1;
                iter.next();
            }
            continue;
        }

        match ch {
            '\'' => single = true,
            '"' => double = true,
            '$' if iter.peek().is_some_and(|(_, next)| *next == '(') => {
                command_sub_depth += 1;
                iter.next();
            }
            '(' if command_sub_depth == 0 => paren_depth += 1,
            ')' if command_sub_depth > 0 => command_sub_depth -= 1,
            ')' if paren_depth > 0 => paren_depth -= 1,
            ';' if paren_depth == 0 && command_sub_depth == 0 => last = None,
            '|' if paren_depth == 0 && command_sub_depth == 0 => {
                if iter.peek().is_some_and(|(_, next)| *next == '|') {
                    iter.next();
                    last = Some(ChainOperator { end: idx + 2 });
                } else {
                    last = None;
                }
            }
            '&' if paren_depth == 0
                && command_sub_depth == 0
                && iter.peek().is_some_and(|(_, next)| *next == '&') =>
            {
                iter.next();
                last = Some(ChainOperator { end: idx + 2 });
            }
            _ => {}
        }
    }
    last
}

