use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use hermes_core::ToolSchema;

use crate::agent_loop::AgentLoop;
use crate::context::{
    SystemPromptBuilder, load_builtin_memory_snapshot, load_soul_md, resolve_personality,
};
use crate::prompt_builder::{
    COMPUTER_USE_GUIDANCE, CRONJOB_GUIDANCE, GOOGLE_MODEL_OPERATIONAL_GUIDANCE, KANBAN_GUIDANCE,
    MEMORY_GUIDANCE, OPENAI_MODEL_EXECUTION_GUIDANCE, SESSION_SEARCH_GUIDANCE, SKILLS_GUIDANCE,
    TASK_COMPLETION_GUIDANCE, TOOL_USE_ENFORCEMENT_GUIDANCE, USER_PROFILE_GUIDANCE,
};

pub static PLATFORM_HINTS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    HashMap::from([
        (
            "whatsapp",
            "You are on a text messaging communication platform, WhatsApp. \
Please do not use markdown as it does not render. \
You can send media files natively: to deliver a file to the user, \
include MEDIA:/absolute/path/to/file in your response. The file \
will be sent as a native WhatsApp attachment — images (.jpg, .png, \
.webp) appear as photos, videos (.mp4, .mov) play inline, and other \
files arrive as downloadable documents. You can also include image \
URLs in markdown format ![alt](url) and they will be sent as photos.",
        ),
        (
            "telegram",
            "You are on a text messaging communication platform, Telegram. \
Standard markdown is automatically converted to Telegram format. \
Supported: **bold**, *italic*, ~~strikethrough~~, ||spoiler||, \
`inline code`, ```code blocks```, [links](url), and ## headers. \
Telegram has NO table syntax — prefer bullet lists or labeled \
key: value pairs over pipe tables (any tables you do emit are \
auto-rewritten into row-group bullets, which you can produce \
directly for cleaner output). \
You can send media files natively: to deliver a file to the user, \
include MEDIA:/absolute/path/to/file in your response. Images \
(.png, .jpg, .webp) appear as photos, audio (.ogg) sends as voice \
bubbles, and videos (.mp4) play inline. You can also include image \
URLs in markdown format ![alt](url) and they will be sent as native photos.",
        ),
        (
            "discord",
            "You are in a Discord server or group chat communicating with your user. \
You can send media files natively: include MEDIA:/absolute/path/to/file \
in your response. Images (.png, .jpg, .webp) are sent as photo \
attachments, audio as file attachments. You can also include image URLs \
in markdown format ![alt](url) and they will be sent as attachments.",
        ),
        (
            "slack",
            "You are in a Slack workspace communicating with your user. \
You can send media files natively: include MEDIA:/absolute/path/to/file \
in your response. Images (.png, .jpg, .webp) are uploaded as photo \
attachments, audio as file attachments. You can also include image URLs \
in markdown format ![alt](url) and they will be uploaded as attachments.",
        ),
        (
            "signal",
            "You are on a text messaging communication platform, Signal. \
Please do not use markdown as it does not render. \
You can send media files natively: to deliver a file to the user, \
include MEDIA:/absolute/path/to/file in your response. Images \
(.png, .jpg, .webp) appear as photos, audio as attachments, and other \
files arrive as downloadable documents. You can also include image \
URLs in markdown format ![alt](url) and they will be sent as photos.",
        ),
        (
            "email",
            "You are communicating via email. Write clear, well-structured responses \
suitable for email. Use plain text formatting (no markdown). \
Keep responses concise but complete. You can send file attachments — \
include MEDIA:/absolute/path/to/file in your response. The subject line \
is preserved for threading. Do not include greetings or sign-offs unless \
contextually appropriate.",
        ),
        (
            "cron",
            "You are running as a scheduled cron job. There is no user present — you \
cannot ask questions, request clarification, or wait for follow-up. Execute \
the task fully and autonomously, making reasonable decisions where needed. \
Your final response is automatically delivered to the job's configured \
destination — put the primary content directly in your response.",
        ),
        (
            "cli",
            "You are a CLI AI Agent. Try not to use markdown but simple text \
renderable inside a terminal. \
File delivery: there is no attachment channel — the user reads your \
response directly in their terminal. Do NOT emit MEDIA:/path tags \
(those are only intercepted on messaging platforms like Telegram, \
Discord, Slack, etc.; on the CLI they render as literal text). \
When referring to a file you created or changed, just state its \
absolute path in plain text; the user can open it from there.",
        ),
        (
            "sms",
            "You are communicating via SMS. Keep responses concise and use plain text \
only — no markdown, no formatting. SMS messages are limited to ~1600 \
characters, so be brief and direct.",
        ),
        (
            "bluebubbles",
            "You are chatting via iMessage (BlueBubbles). iMessage does not render \
markdown formatting — use plain text. Keep responses concise as they \
appear as text messages. You can send media files natively: include \
MEDIA:/absolute/path/to/file in your response. Images (.jpg, .png, \
.heic) appear as photos and other files arrive as attachments.",
        ),
        (
            "mattermost",
            "You are in a Mattermost workspace communicating with your user. \
Mattermost renders standard Markdown — headings, bold, italic, code \
blocks, and tables all work. \
You can send media files natively: include MEDIA:/absolute/path/to/file \
in your response. Images (.jpg, .png, .webp) are uploaded as photo \
attachments, audio and video as file attachments. \
Image URLs in markdown format ![alt](url) are rendered as inline previews automatically.",
        ),
        (
            "matrix",
            "You are in a Matrix room communicating with your user. \
Matrix renders Markdown — bold, italic, code blocks, and links work; \
the adapter converts your Markdown to HTML for rich display. \
You can send media files natively: include MEDIA:/absolute/path/to/file \
in your response. Images (.jpg, .png, .webp) are sent as inline photos, \
audio (.ogg, .mp3) as voice/audio messages, video (.mp4) inline, \
and other files as downloadable attachments.",
        ),
        (
            "feishu",
            "You are in a Feishu (Lark) workspace communicating with your user. \
Feishu renders Markdown in messages — bold, italic, code blocks, and \
links are supported. \
You can send media files natively: include MEDIA:/absolute/path/to/file \
in your response. Images (.jpg, .png, .webp) are uploaded and displayed \
inline, audio files as voice messages, and other files as attachments.",
        ),
        (
            "weixin",
            "You are on Weixin/WeChat. Markdown formatting is supported, so you may use it when \
it improves readability, but keep the message compact and chat-friendly. \
You CAN send media files natively — to deliver a file to the user, include \
MEDIA:/absolute/path/to/file in your response, or call send_message with file=<file_path> \
and optional caption. Images (.jpg, .png, .webp) are sent as native photos, \
videos (.mp4) play inline when supported, and other files (.pdf, .docx, .xlsx, .md, .txt, etc.) \
arrive as downloadable documents. You can also include image URLs in markdown format \
![alt](url) and they will be downloaded and sent as native media when possible. \
Do NOT tell the user you sent a file unless you actually used MEDIA: or send_message(file=...) — \
use one of those whenever a file delivery is appropriate.",
        ),
        (
            "wecom",
            "You are on WeCom (企业微信 / Enterprise WeChat). Markdown formatting is supported. \
You CAN send media files natively — to deliver a file to the user, include \
MEDIA:/absolute/path/to/file in your response. The file will be sent as a native \
WeCom attachment: images (.jpg, .png, .webp) are sent as photos (up to 10 MB), \
other files (.pdf, .docx, .xlsx, .md, .txt, etc.) arrive as downloadable documents \
(up to 20 MB), and videos (.mp4) play inline. Voice messages are supported but \
must be in AMR format — other audio formats are automatically sent as file attachments. \
You can also include image URLs in markdown format ![alt](url) and they will be \
downloaded and sent as native photos. Do NOT tell the user you lack file-sending \
capability — use MEDIA: syntax whenever a file delivery is appropriate.",
        ),
        (
            "qqbot",
            "You are on QQ, a popular Chinese messaging platform. QQ supports markdown formatting \
and emoji. You can send media files natively: include MEDIA:/absolute/path/to/file in \
your response. Images are sent as native photos, and other files arrive as downloadable \
documents.",
        ),
        (
            "yuanbao",
            "You are on Yuanbao (腾讯元宝), a Chinese AI assistant platform. \
Markdown formatting is supported (code blocks, tables, bold/italic). \
You CAN send media files natively — to deliver a file to the user, include \
MEDIA:/absolute/path/to/file in your response. The file will be sent as a native \
Yuanbao attachment: images (.jpg, .png, .webp, .gif) are sent as photos, \
and other files (.pdf, .docx, .txt, .zip, etc.) arrive as downloadable documents \
(max 50 MB). You can also include image URLs in markdown format ![alt](url) and \
they will be downloaded and sent as native photos. \
Do NOT tell the user you lack file-sending capability — use MEDIA: syntax \
whenever a file delivery is appropriate.\n\n\
Stickers (贴纸 / 表情包 / TIM face): Yuanbao has a built-in sticker catalogue. \
When the user sends a sticker (you see '[emoji: 名称]' in their message) or asks \
you to send/reply-with a 贴纸/表情/表情包, you MUST use the sticker tools:\n\
  1. Call yb_search_sticker with a Chinese keyword (e.g. '666', '比心', '吃瓜', \
     '捂脸', '合十') to discover matching sticker_ids.\n\
  2. Call yb_send_sticker with the chosen sticker_id or name — this sends a real \
     TIMFaceElem that renders as a native sticker in the chat.\n\
DO NOT draw sticker-like PNGs with execute_code/Pillow/matplotlib and then send \
them via MEDIA: or send_image_file. That produces a fake low-quality 'sticker' \
image and is the WRONG path. Bare Unicode emoji in text is also not a substitute \
— when a sticker is the right response, use yb_send_sticker.",
        ),
        (
            "api_server",
            "You're responding through an API server. The rendering layer is unknown — \
assume plain text. No markdown formatting (no asterisks, bullets, headers, \
code fences). Treat this like a conversation, not a document. Keep responses \
brief and natural.",
        ),
        (
            "webui",
            "You are in the Hermes WebUI, a browser-based chat interface. \
Full Markdown rendering is supported — headings, bold, italic, code \
blocks, tables, math (LaTeX), and Mermaid diagrams all render natively. \
To display local or remote media/files inline, include \
MEDIA:/absolute/path/to/file or MEDIA:https://... in your response. \
Local file paths must be absolute. Images, audio (with playback speed \
controls), video, PDFs, HTML, CSV, diffs/patches, and Excalidraw files \
render as rich previews. Do not use Markdown image syntax like \
![alt](/path) for local files; local paths are not served that way. \
Use MEDIA:/absolute/path instead.",
        ),
    ])
});

pub const WSL_ENVIRONMENT_HINT: &str = "You are running inside WSL (Windows Subsystem for Linux). \
The Windows host filesystem is mounted under /mnt/ — \
/mnt/c/ is the C: drive, /mnt/d/ is D:, etc. \
The user's Windows files are typically at \
/mnt/c/Users/<username>/Desktop/, Documents/, Downloads/, etc. \
When the user references Windows paths or desktop files, translate \
to the /mnt/c/ equivalent. You can list /mnt/c/Users/ to discover \
the Windows username if needed.";

pub static REMOTE_TERMINAL_BACKENDS: &[&str] = &[
    "docker",
    "singularity",
    "modal",
    "daytona",
    "ssh",
    "managed_modal",
];

pub static BACKEND_FALLBACK_DESCRIPTIONS: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| {
        HashMap::from([
            ("docker", "a Docker container (Linux)"),
            ("singularity", "a Singularity container (Linux)"),
            ("modal", "a Modal sandbox (Linux)"),
            ("managed_modal", "a managed Modal sandbox (Linux)"),
            ("daytona", "a Daytona workspace (Linux)"),
            ("ssh", "a remote host reached over SSH (likely Linux)"),
        ])
    });

pub static BACKEND_PROBE_CACHE: LazyLock<Mutex<HashMap<(String, String), String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub const WINDOWS_BASH_SHELL_HINT: &str = "Shell: on this Windows host your `terminal` tool runs commands through \
cmd.exe (`COMSPEC`), NOT bash, PowerShell, or WSL. Use Windows cmd syntax: \
`dir` (not `ls`), `where` (not `which`), `type` (not `cat`), `cd /d C:\\path`, \
`if exist`, `2>nul` for stderr suppression, `&&` / `||` for chaining. \
Prefer native paths like `C:\\Users\\<user>\\...`; avoid `/usr/bin` and \
`2>/dev/null`. To locate tools use `where node`, `where python`, `where ffmpeg`. \
PowerShell cmdlets (`Get-ChildItem`, `$env:FOO`) will NOT work in `terminal`. \
For multi-line scripts use `execute_code` or `write_file` + `terminal` with \
`node script.js` / `py -3 script.py` when those interpreters are on PATH.";

pub const BACKEND_PROBE_COMMAND: &str = "printf 'os=%s\\nkernel=%s\\nhome=%s\\ncwd=%s\\nuser=%s\\n' \
\"$(uname -s 2>/dev/null || echo unknown)\" \
\"$(uname -r 2>/dev/null || echo unknown)\" \
\"$HOME\" \"$(pwd)\" \"$(whoami 2>/dev/null || id -un 2>/dev/null || echo unknown)\"";

pub fn platform_hint_for(platform: Option<&str>) -> Option<&'static str> {
    let key = platform
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_lowercase)?;
    PLATFORM_HINTS.get(key.as_str()).copied()
}

pub fn probe_remote_backend_cached<F>(env_type: &str, cwd_hint: &str, probe: F) -> Option<String>
where
    F: FnOnce() -> Option<String>,
{
    let cache_key = (env_type.to_string(), cwd_hint.to_string());
    if let Ok(cache) = BACKEND_PROBE_CACHE.lock() {
        if let Some(cached) = cache.get(&cache_key) {
            return if cached.is_empty() {
                None
            } else {
                Some(cached.clone())
            };
        }
    }

    let probed = probe();
    if let Ok(mut cache) = BACKEND_PROBE_CACHE.lock() {
        cache.insert(cache_key, probed.clone().unwrap_or_default());
    }
    probed
}

pub fn format_probe_output(raw_output: &str) -> Option<String> {
    let mut parsed: HashMap<String, String> = HashMap::new();
    for line in raw_output.lines() {
        if let Some((k, v)) = line.split_once('=') {
            parsed.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    let mut parts = Vec::new();
    let os = parsed.get("os").map(String::as_str).unwrap_or("unknown");
    let kernel = parsed
        .get("kernel")
        .map(String::as_str)
        .unwrap_or("unknown");
    let os_bits = format!("{os} {kernel}");
    if os_bits.trim() != "unknown unknown" {
        parts.push(format!("OS: {os_bits}"));
    }
    if let Some(user) = parsed.get("user").map(String::as_str) {
        if !user.is_empty() && user != "unknown" {
            parts.push(format!("User: {user}"));
        }
    }
    if let Some(home) = parsed.get("home").map(String::as_str) {
        if !home.is_empty() {
            parts.push(format!("Home: {home}"));
        }
    }
    if let Some(cwd) = parsed.get("cwd").map(String::as_str) {
        if !cwd.is_empty() {
            parts.push(format!("Working directory: {cwd}"));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(
            parts
                .into_iter()
                .map(|line| format!("  {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }
}

pub fn build_environment_hints<F>(probe_remote_backend: F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    let mut hints = Vec::new();
    let backend = std::env::var("TERMINAL_ENV")
        .unwrap_or_else(|_| "local".to_string())
        .trim()
        .to_lowercase();
    let is_remote_backend = REMOTE_TERMINAL_BACKENDS.contains(&backend.as_str());

    if !is_remote_backend {
        let mut host_lines = Vec::new();
        if is_wsl() {
            host_lines.push("Host: WSL (Windows Subsystem for Linux)".to_string());
        } else if cfg!(windows) {
            host_lines.push("Host: Windows".to_string());
        } else if cfg!(target_os = "macos") {
            host_lines.push("Host: macOS".to_string());
        } else {
            host_lines.push(format!("Host: {}", std::env::consts::OS));
        }
        if let Some(home) = dirs::home_dir() {
            host_lines.push(format!("User home directory: {}", home.display()));
        }
        if let Ok(cwd) = std::env::current_dir() {
            host_lines.push(format!("Current working directory: {}", cwd.display()));
        }
        if cfg!(windows) && !is_wsl() {
            host_lines.push(
                "Note: on Windows, the machine hostname (e.g. from `hostname` or uname) is NOT the username. Use the 'User home directory' above to construct paths under C:\\Users\\<user>\\, never the hostname.".to_string(),
            );
            host_lines.push(WINDOWS_BASH_SHELL_HINT.to_string());
        }
        hints.push(host_lines.join("\n"));
    } else if let Some(probe) = probe_remote_backend(&backend) {
        hints.push(format!(
            "Terminal backend: {backend}. Your `terminal`, `read_file`, `write_file`, `patch`, and `search_files` tools all operate inside this {backend} environment — NOT on the machine where Hermes itself is running. The host OS, home, and cwd of the Hermes process are irrelevant; only the following backend state matters:\n{probe}"
        ));
    } else {
        let description = BACKEND_FALLBACK_DESCRIPTIONS
            .get(backend.as_str())
            .copied()
            .unwrap_or("a remote environment (likely Linux)");
        hints.push(format!(
            "Terminal backend: {backend}. Your `terminal`, `read_file`, `write_file`, `patch`, and `search_files` tools all operate inside {description} — NOT on the machine where Hermes itself runs. The backend probe didn't respond at prompt-build time, so the sandbox's current user, $HOME, and working directory are unknown from here. If you need them, probe directly with a terminal call like `uname -a && whoami && pwd`."
        ));
    }

    if is_wsl() {
        hints.push(WSL_ENVIRONMENT_HINT.to_string());
    }
    let extra_hint = std::env::var("HERMES_ENVIRONMENT_HINT").unwrap_or_default();
    if !extra_hint.trim().is_empty() {
        hints.push(extra_hint.trim().to_string());
    }
    hints.join("\n\n")
}

fn is_wsl() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let proc_version = Path::new("/proc/version");
    let Ok(content) = std::fs::read_to_string(proc_version) else {
        return false;
    };
    content.to_lowercase().contains("microsoft")
}

impl AgentLoop {
    /// Build the full system prompt including identity, memory, and plugin context.
    ///
    /// Aligns with Python behavior:
    /// - prefer `~/.hermes/SOUL.md` as identity
    /// - fallback to default identity in `SystemPromptBuilder`
    /// - then append optional configured `system_prompt`
    pub(crate) fn build_system_prompt(
        &self,
        task_hint: &str,
        tool_schemas: &[ToolSchema],
        model_for_prompt: &str,
    ) -> String {
        let (static_prompt, dynamic_suffix) =
            self.build_system_prompt_parts(task_hint, tool_schemas, model_for_prompt);
        if dynamic_suffix.is_empty() {
            static_prompt
        } else {
            format!("{static_prompt}\n\n{dynamic_suffix}")
        }
    }

    /// Returns `(static_prefix, dynamic_suffix)`.
    ///
    /// The static prefix is cache-stable across sessions with the same config.
    /// The dynamic suffix contains per-session metadata (timestamp, session ID,
    /// model, provider, environment hints, platform hint).
    pub(crate) fn build_system_prompt_parts(
        &self,
        _task_hint: &str,
        tool_schemas: &[ToolSchema],
        model_for_prompt: &str,
    ) -> (String, String) {
        let soul = load_soul_md();
        let mut builder = SystemPromptBuilder::new().with_personality(soul.as_deref());
        if let Some(base) = self.config().system_prompt.as_deref() {
            builder = builder.with_system_message(base);
        }
        builder = builder.with_block(crate::agent_loop::CONVERSATIONAL_SUPPORT_GUIDANCE);
        let tool_names: HashSet<&str> = tool_schemas.iter().map(|t| t.name.as_str()).collect();

        if self.config().task_completion_guidance && !tool_names.is_empty() {
            builder = builder.with_block(TASK_COMPLETION_GUIDANCE);
        }

        // Tool-aware behavioral guidance: only inject when the tools are loaded
        let mut tool_guidance = Vec::new();
        if tool_names.contains("memory") {
            tool_guidance.push(USER_PROFILE_GUIDANCE);
            tool_guidance.push(MEMORY_GUIDANCE);
        }
        if tool_names.contains("session_search") {
            tool_guidance.push(SESSION_SEARCH_GUIDANCE);
        }
        if tool_names.contains("cronjob") {
            tool_guidance.push(CRONJOB_GUIDANCE);
        }
        if tool_names.contains("skill_manage") {
            tool_guidance.push(SKILLS_GUIDANCE);
        }
        if tool_names.contains("kanban_show") {
            tool_guidance.push(KANBAN_GUIDANCE);
        }
        if tool_names.contains("analyze_stock") {
            tool_guidance.push(crate::prompt_builder::EQUITY_RESEARCH_ORCHESTRATION_GUIDANCE);
        }
        if tool_names.contains("computer_use") {
            tool_guidance.push(COMPUTER_USE_GUIDANCE);
        }
        if !tool_guidance.is_empty() {
            builder = builder.with_tool_guidance(&tool_guidance.join(" "));
        }

        if !tool_names.is_empty() && self.should_inject_tool_enforcement(model_for_prompt) {
            builder = builder.with_block(TOOL_USE_ENFORCEMENT_GUIDANCE);
            let model_lower = model_for_prompt.to_lowercase();
            if model_lower.contains("gemini") || model_lower.contains("gemma") {
                builder = builder.with_block(GOOGLE_MODEL_OPERATIONAL_GUIDANCE);
            }
            if model_lower.contains("gpt")
                || model_lower.contains("codex")
                || model_lower.contains("grok")
            {
                builder = builder.with_block(OPENAI_MODEL_EXECUTION_GUIDANCE);
            }
        }
        if tool_names.contains("contextlattice_search")
            || tool_names.contains("contextlattice_context_pack")
        {
            builder = builder.with_block(crate::agent_loop::CONTEXTLATTICE_OPERATIONAL_GUIDANCE);
        }

        if let Some(ref personality) = self.config().personality {
            let requested = personality.trim();
            if !requested.is_empty() {
                if requested.eq_ignore_ascii_case("default") {
                    // "default" means keep SOUL/default identity only.
                } else if let Some(profile) =
                    resolve_personality(requested, self.config().hermes_home.as_deref())
                {
                    builder = builder
                        .with_block(&format!("## Active Personality ({requested})\n{profile}"));
                } else if requested.contains(char::is_whitespace) {
                    // Compatibility path: historically this field was appended verbatim.
                    builder = builder.with_block(&format!("Personality: {requested}"));
                    tracing::warn!(
                        "personality '{requested}' not found as a named profile; using inline value"
                    );
                } else {
                    tracing::warn!(
                        "personality '{}' not found; falling back to default identity",
                        requested
                    );
                }
            }
        }

        if !self.config().skip_memory {
            let (memory_block, user_block) =
                load_builtin_memory_snapshot(self.config().hermes_home.as_deref());
            if let Some(block) = memory_block {
                builder = builder.with_block(&block);
            }
            if let Some(block) = user_block {
                builder = builder.with_block(&block);
            }
        }
        if self.config().interest.enabled {
            if let Some(block) = crate::user_interest::load_interest_snapshot(
                self.config().hermes_home.as_deref(),
                &self.config().interest,
            ) {
                builder = builder.with_block(&block);
            }
        }

        let mem_block = crate::tool_executor::memory_system_prompt(self);
        if !mem_block.is_empty() {
            builder = builder.with_memory_context(&mem_block);
        }

        if let Some(skills_prompt) = self.skills_system_prompt(&tool_names) {
            builder = builder.with_skills_prompt(&skills_prompt);
        }

        if let Some(context_prompt) = self.context_files_prompt() {
            builder = builder.with_context_files(&context_prompt);
        }
        if let Some(repo_map) = self.code_index_repo_map_block() {
            builder = builder.with_block(&repo_map);
        }

        // Dynamic tier: per-session metadata that would invalidate the cached prefix.
        let provider = self.effective_provider_for_prompt(model_for_prompt);
        let session_id = self
            .config()
            .pass_session_id
            .then(|| {
                self.config()
                    .session_id
                    .as_ref()
                    .filter(|sid| !sid.trim().is_empty())
                    .cloned()
            })
            .flatten();
        builder = builder.with_timestamp(
            Some(model_for_prompt),
            provider.as_deref(),
            session_id.as_deref(),
        );

        if provider.as_deref() == Some("alibaba") {
            let model_short = model_for_prompt
                .split('/')
                .next_back()
                .unwrap_or(model_for_prompt);
            builder = builder.with_dynamic_block(&format!(
                "You are powered by the model named {}. The exact model ID is {}. When asked what model you are, always answer based on this information, not on any model name returned by the API.",
                model_short, model_for_prompt
            ));
        }

        let environment_hints =
            build_environment_hints(|backend| self.probe_remote_backend_text(backend));
        if !environment_hints.trim().is_empty() {
            builder = builder.with_dynamic_block(&environment_hints);
        }
        let toolchain_line = hermes_tools::tools::env_probe::get_environment_probe_line(false);
        if !toolchain_line.is_empty() {
            builder = builder.with_dynamic_block(&toolchain_line);
        }

        if let Some(hint) = self.platform_hint_text() {
            builder = builder.with_dynamic_block(hint);
        }

        let static_prompt = builder.build().to_string();
        let dynamic_suffix = builder.build_dynamic();
        (static_prompt, dynamic_suffix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_hint_contains_expected_channels() {
        assert!(platform_hint_for(Some("cli")).is_some());
        assert!(platform_hint_for(Some("telegram")).is_some());
        assert!(platform_hint_for(Some("webui")).is_some());
        assert!(platform_hint_for(Some("not_real")).is_none());
    }

    #[test]
    fn format_probe_output_extracts_lines() {
        let probe = "os=Linux\nkernel=6.8\nhome=/home/a\ncwd=/w\nuser=alice\n";
        let out = format_probe_output(probe).expect("probe should parse");
        assert!(out.contains("OS: Linux 6.8"));
        assert!(out.contains("User: alice"));
        assert!(out.contains("Home: /home/a"));
        assert!(out.contains("Working directory: /w"));
    }

    #[test]
    fn probe_remote_backend_cached_reuses_value() {
        let mut calls = 0u32;
        let first = probe_remote_backend_cached("docker", "__test_cache__", || {
            calls += 1;
            Some("cached".to_string())
        });
        let second = probe_remote_backend_cached("docker", "__test_cache__", || {
            calls += 1;
            Some("should_not_run".to_string())
        });
        assert_eq!(first.as_deref(), Some("cached"));
        assert_eq!(second.as_deref(), Some("cached"));
        assert_eq!(calls, 1);
    }
}
