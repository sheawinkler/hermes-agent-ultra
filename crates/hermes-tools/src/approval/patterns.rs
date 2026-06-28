static DENIED_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)\brm\s+--no-preserve-root\s").unwrap(),
        Regex::new(
            r"(?is)\bpython(?:3(?:\.\d+)?)?\s+-c\s+.*(shutil\.rmtree|os\.(remove|unlink))\s*\(",
        )
        .unwrap(),
        Regex::new(r"(?i)\b(shred|wipefs)\b").unwrap(),
        Regex::new(r"(?i):()\s*>\s*/dev/").unwrap(),
        Regex::new(r"(?i)>\s*/dev/sd[a-z]").unwrap(),
    ]
});

/// Patterns that require confirmation.
static CONFIRM_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // sudo commands
        Regex::new(r"(?i)\bsudo\b").unwrap(),
        // rm -r (but not rm -rf which is denied)
        Regex::new(r"(?i)\brm\s+-(?:[A-Za-z]*r|[A-Za-z]*r[A-Za-z]*f|[A-Za-z]*f[A-Za-z]*r)").unwrap(),
        Regex::new(r"(?i)\brm\s+--recursive\b").unwrap(),
        // System service manipulation
        Regex::new(r"(?i)\bsystemctl\s+(start|stop|restart|enable|disable)\s").unwrap(),
        // Package management
        Regex::new(r"(?i)\b(apt|apt-get|yum|dnf|pacman|brew)\s+(install|remove|purge)\b").unwrap(),
        // Network configuration
        Regex::new(r"(?i)\biptables\b").unwrap(),
        Regex::new(r"(?i)\bifconfig\s").unwrap(),
        // Process killing
        Regex::new(r"(?i)\bkill\s+-9\b").unwrap(),
        Regex::new(r"(?i)\bkillall\s+(?:-[A-Za-z]*9|-[A-Za-z]*KILL|-[A-Za-z]*SIGKILL|-s\s+(?:9|KILL)|-r\b)").unwrap(),
        // Disk operations
        Regex::new(r"(?i)\bformat\b").unwrap(),
        Regex::new(r"(?is)\bdd\s+.*(?:if=/dev/|of=)").unwrap(),
        Regex::new(r"(?i)\bchmod\s+(?:-[A-Za-z]*R[A-Za-z]*\s+|--recursive\s+)?777\s").unwrap(),
        // Cron modifications
        Regex::new(r"(?i)\bcrontab\s+-r\b").unwrap(),
        // SQL destructive operations
        Regex::new(r"(?i)\bdrop\s+table\b").unwrap(),
        // Shell via command string
        Regex::new(r"(?is)\b(?:bash|sh|zsh|ksh)\s+-l?c\b").unwrap(),
        // Shell pipe to sh
        Regex::new(r"\|\s*(ba)?sh\b").unwrap(),
        // Curl pipe to shell
        // DOTALL hardening: catch multiline curl payloads piped to shell.
        Regex::new(r"(?is)curl\s+.*\|\s*(ba)?sh\b").unwrap(),
        Regex::new(r"(?is)wget\s+.*\|\s*(ba)?sh\b").unwrap(),
        // Remote script process substitution
        Regex::new(r"(?is)\b(?:bash|sh|zsh|ksh)\s+<\s*(?:<\s*)?\(\s*(?:curl|wget)\b").unwrap(),
        // Gateway restarts should go through the service manager so agents do
        // not strand unmanaged background gateway processes.
        Regex::new(r"(?is)\bgateway\s+run\b.*(?:--replace\b|&\s*disown\b|disown\b|&\s*$)")
            .unwrap(),
        // Writing to system directories
        Regex::new(r"(?i)(?:>|>>)\s*/(?:private/)?(?:etc|usr|var|boot|bin)/").unwrap(),
        Regex::new(r"(?i)\|\s*tee\s+/(?:private/)?(?:etc|usr|var|boot|bin)/").unwrap(),
        Regex::new(r"(?i)\b(?:cp|mv|install)\b.*\s/(?:private/)?(?:etc|usr|var|boot|bin)/").unwrap(),
        Regex::new(r"(?i)\bsed\s+(?:-[^\s]*i|--in-place)\b.*\s/(?:private/)?(?:etc|usr|var|boot|bin)/").unwrap(),
        // Project/user managed sensitive files.
        Regex::new(r##"(?i)(?:>|>>)\s*(?:"?\$HERMES_HOME/?|"?\$HOME/?|~/?)(?:\.hermes/)?(?:\.env|\.ssh/authorized_keys)"?"##).unwrap(),
        Regex::new(r#"(?i)(?:>|>>)\s*(?:/?[\w./-]*\.env(?:\.[\w-]+)?|[\w./-]*config\.(?:ya?ml|json|toml))\b"#).unwrap(),
        Regex::new(r#"(?i)\|\s*tee\s+(?:"?\$HERMES_HOME/?|"?\$HOME/?|~/?)?(?:\.hermes/)?(?:\.env(?:\.[\w-]+)?|\.ssh/authorized_keys|[\w./-]*config\.(?:ya?ml|json|toml))"#).unwrap(),
        Regex::new(r#"(?i)\b(?:cp|mv|install)\b.*\s(?:\.env(?:\.[\w-]+)?|/[\w./-]+/\.env(?:\.[\w-]+)?|[\w./-]*config\.(?:ya?ml|json|toml))\s*$"#).unwrap(),
        // Docker operations that affect system
        Regex::new(r"(?i)\bdocker\s+(rm|rmi|system\s+prune)\b").unwrap(),
        // Git force push
        Regex::new(r"(?is)\bgit\s+push\s+.*--force\b").unwrap(),
        Regex::new(r"(?i)\bgit\s+push\s+-f\b").unwrap(),
        // Destructive git tree operations
        Regex::new(r"(?i)\bgit\s+reset\s+--hard\b").unwrap(),
        Regex::new(r"(?i)\bgit\s+clean\s+-[^\n]*f[^\n]*d[^\n]*x").unwrap(),
        // find destructive execution/deletion
        Regex::new(r"(?i)\bfind\b.*-exec(?:dir)?\s+(?:/(?:usr/)?bin/)?rm\b").unwrap(),
        Regex::new(r"(?i)\bfind\b.*\s-delete\b").unwrap(),
    ]
});

static HARDLINE_RM_PROTECTED_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\brm\s+(?:-[A-Za-z]*r[A-Za-z]*f[A-Za-z]*|-[A-Za-z]*f[A-Za-z]*r[A-Za-z]*|--recursive\s+--force|--force\s+--recursive)\s+(?:/|/\*|/(?:home|etc|usr|var|boot|bin)(?:/\*)?|~(?:/|/\*|\*)?|\$HOME)(?:\s|$)",
    )
    .unwrap()
});

static BLOCK_DEVICE_PATH: &str = r"/dev/(?:sd[a-z]\d*|hd[a-z]\d*|nvme\d+n\d+(?:p\d+)?)\b";

static HARDLINE_MKFS_BLOCK_DEVICE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)\bmkfs(?:\.[A-Za-z0-9_+-]+)?\s+{BLOCK_DEVICE_PATH}"
    ))
    .unwrap()
});

static HARDLINE_DD_BLOCK_DEVICE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?is)\bdd\b.*\bof={BLOCK_DEVICE_PATH}")).unwrap());

static HARDLINE_REDIRECT_BLOCK_DEVICE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?is)(?:>|>>)\s*{BLOCK_DEVICE_PATH}")).unwrap());

static HARDLINE_KILL_ALL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bkill\s+(?:-9\s+)?-1\b").unwrap());

static HARDLINE_STOP_SYSTEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        (?:^|;|&&|\|\||`|\$\()\s*
        (?:
            (?:sudo(?:\s+-[A-Za-z0-9_=/-]+)*\s+)?
            (?:env(?:\s+[A-Za-z_][A-Za-z0-9_]*=\S+)*\s+)?
            (?:(?:exec|nohup|setsid)\s+)?
        )
        (?:
            shutdown\b|reboot\b|halt\b|poweroff\b|
            (?:init|telinit)\s+(?:0|6)\b|
            systemctl\s+(?:poweroff|reboot|halt)\b
        )
        ",
    )
    .unwrap()
});

static SUDO_STDIN_GUARD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[;&|]\s*)\bsudo\b[^;&|\n]*(?:\s--stdin\b|\s--askpass\b|\s-[A-Za-z]*[SAas][A-Za-z]*\b)")
        .unwrap()
});

static DELETE_FROM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bdelete\s+from\b").unwrap());

static CONTAINER_BACKENDS: &[&str] = &["docker", "singularity", "modal", "daytona"];

struct CommandPatternRule {
    regex: Regex,
    key: &'static str,
    description: &'static str,
}

impl CommandPatternRule {
    fn new(pattern: &str, key: &'static str, description: &'static str) -> Self {
        Self {
            regex: Regex::new(pattern).unwrap(),
            key,
            description,
        }
    }
}

static DANGEROUS_COMMAND_RULES: LazyLock<Vec<CommandPatternRule>> = LazyLock::new(|| {
    vec![
        CommandPatternRule::new(
            r"(?i)\brm\s+-[A-Za-z]*r",
            "recursive delete",
            "recursive delete",
        ),
        CommandPatternRule::new(
            r"(?i)\brm\s+--recursive\b",
            "recursive delete (long flag)",
            "recursive delete (long flag)",
        ),
        CommandPatternRule::new(
            r"(?i)\bchmod\s+(?:-[A-Za-z]*R[A-Za-z]*\s+|--recursive\s+)?(?:777|666|o\+[rwx]*w|a\+[rwx]*w)\b",
            "world/other-writable permissions",
            "world/other-writable permissions",
        ),
        CommandPatternRule::new(
            r"(?i)\bsystemctl\s+(?:-[^\s]+\s+)*(?:stop|restart|disable|mask)\b",
            "stop/restart system service",
            "stop/restart system service",
        ),
        CommandPatternRule::new(r"(?is)\bdd\s+.*(?:if=|of=)", "disk copy", "disk copy"),
        CommandPatternRule::new(r"(?i)\bdrop\s+(?:table|database)\b", "SQL DROP", "SQL DROP"),
        CommandPatternRule::new(
            r"(?i)\btruncate\s+(?:table\s+)?\w",
            "SQL TRUNCATE",
            "SQL TRUNCATE",
        ),
        CommandPatternRule::new(
            r"(?i)\b(?:bash|sh|zsh|ksh)\s+-[^\s]*c(?:\s+|$)",
            "shell command via -c/-lc flag",
            "shell command via -c/-lc flag",
        ),
        CommandPatternRule::new(
            r"(?is)\b(?:curl|wget)\b.*\|\s*(?:[/\w]*/)?(?:ba)?sh(?:\s|$|-c)",
            "pipe remote content to shell",
            "pipe remote content to shell",
        ),
        CommandPatternRule::new(
            r"(?is)\b(?:bash|sh|zsh|ksh)\s+<\s*(?:<\s*)?\(\s*(?:curl|wget)\b",
            "execute remote script via process substitution",
            "execute remote script via process substitution",
        ),
        CommandPatternRule::new(
            r"(?is)\bgateway\s+run\b.*(?:--replace\b|&\s*disown\b|disown\b|&\s*$)",
            "unmanaged gateway process start",
            "gateway process should be restarted through systemctl/service management",
        ),
        CommandPatternRule::new(
            r"(?i)(?:>|>>)\s*/(?:private/)?(?:etc|usr|var|boot|bin)/",
            "overwrite system config",
            "overwrite system config",
        ),
        CommandPatternRule::new(
            r"(?i)\|\s*tee\s+/(?:private/)?(?:etc|usr|var|boot|bin)/",
            "overwrite system file via tee",
            "overwrite system file via tee",
        ),
        CommandPatternRule::new(
            r##"(?i)(?:>|>>)\s*(?:"?\$HERMES_HOME/?|"?\$HOME/?|~/?)(?:\.hermes/)?(?:\.env|\.ssh/authorized_keys)"?"##,
            "overwrite project env/config via redirection",
            "overwrite project env/config via redirection",
        ),
        CommandPatternRule::new(
            r#"(?i)\|\s*tee\s+(?:"?\$HERMES_HOME/?|"?\$HOME/?|~/?)?(?:\.hermes/)?(?:\.env(?:\.[\w-]+)?|\.ssh/authorized_keys|[\w./-]*config\.(?:ya?ml|json|toml))"#,
            "overwrite project env/config via tee",
            "overwrite project env/config via tee",
        ),
        CommandPatternRule::new(
            r#"(?i)\b(?:cp|mv|install)\b.*\s(?:\.env(?:\.[\w-]+)?|/[\w./-]+/\.env(?:\.[\w-]+)?|[\w./-]*config\.(?:ya?ml|json|toml))\s*$"#,
            "overwrite project env/config file",
            "overwrite project env/config file",
        ),
        CommandPatternRule::new(
            r"(?i)\bdocker\s+(?:compose\s+)?(?:restart|stop|kill|down)\b",
            "docker restart/stop/kill (container lifecycle)",
            "docker restart/stop/kill (container lifecycle)",
        ),
        CommandPatternRule::new(
            r"(?i)\bdocker\s+(?:rm|rmi|system\s+prune)\b",
            "docker destructive operation",
            "docker destructive operation",
        ),
        CommandPatternRule::new(
            r"(?i)\bgit\s+reset\s+--hard\b",
            "git reset --hard (destroys uncommitted changes)",
            "git reset --hard (destroys uncommitted changes)",
        ),
        CommandPatternRule::new(
            r"(?is)\bgit\s+push\s+.*--force\b",
            "git force push (rewrites remote history)",
            "git force push (rewrites remote history)",
        ),
        CommandPatternRule::new(
            r"(?i)\bgit\s+push\s+-f\b",
            "git force push short flag (rewrites remote history)",
            "git force push short flag (rewrites remote history)",
        ),
        CommandPatternRule::new(
            r"(?i)\bgit\s+clean\s+-[^\n]*f",
            "git clean with force (deletes untracked files)",
            "git clean with force (deletes untracked files)",
        ),
        CommandPatternRule::new(
            r"(?i)\bfind\b.*-exec(?:dir)?\s+(?:/(?:usr/)?bin/)?rm\b",
            "find -exec/-execdir rm",
            "find -exec/-execdir rm",
        ),
        CommandPatternRule::new(r"(?i)\bfind\b.*\s-delete\b", "find -delete", "find -delete"),
        CommandPatternRule::new(
            r"(?i)\bkillall\s+(?:-[A-Za-z]*9|-[A-Za-z]*KILL|-[A-Za-z]*SIGKILL|-s\s+(?:9|KILL)|-r\b)",
            "force kill processes (killall)",
            "force kill processes (killall)",
        ),
        CommandPatternRule::new(
            r"(?i)\bkill\s+-9\b",
            "force kill processes",
            "force kill processes",
        ),
        CommandPatternRule::new(
            r"(?i)\bsudo\b[^;&|\n]*(?:\s--stdin\b|\s--askpass\b|\s-[A-Za-z]*[SAas][A-Za-z]*\b)",
            "sudo with privilege flag (stdin/askpass/shell/list)",
            "sudo with privilege flag (stdin/askpass/shell/list)",
        ),
    ]
});

static SESSION_YOLO: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static SESSION_APPROVED: LazyLock<Mutex<HashMap<String, HashSet<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static PERMANENT_APPROVED: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
type GatewayNotifyCallback = Arc<dyn Fn(GatewayApprovalRequest) + Send + Sync + 'static>;
type ApprovalObserverCallback = Arc<dyn Fn(ApprovalHookEvent) + Send + Sync + 'static>;
static GATEWAY_QUEUES: LazyLock<Mutex<HashMap<String, VecDeque<Arc<GatewayApprovalEntry>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static GATEWAY_NOTIFY_CBS: LazyLock<Mutex<HashMap<String, GatewayNotifyCallback>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static APPROVAL_OBSERVERS: LazyLock<Mutex<HashMap<u64, ApprovalObserverCallback>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_APPROVAL_OBSERVER_ID: AtomicU64 = AtomicU64::new(1);
