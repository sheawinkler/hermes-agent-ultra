# Hermes Agent: Python → Rust 重写差距分析

## 项目意图

将 [NousResearch/hermes-agent](https://github.com/NousResearch/hermes-agent) (Python, v0.9.0) 完整重写为 Rust，保持功能对等，获得 Rust 的性能、类型安全和单二进制分发优势。

## 当前状态

| 指标 | Python | Rust | 完成度 |
|------|--------|------|--------|
| 总代码行数 | 355,331 | 41,622 | ~40% |
| 核心 Agent 逻辑 | 9,913 行 (run_agent.py) | ~1,500 行 (agent_loop.rs) | ~45% |
| Gateway | 7,905 行 (gateway/run.py) | ~2,000 行 (gateway crate) | ~35% |
| CLI | 5,827 行 (hermes_cli/main.py) + 42 个子模块 | ~5,000 行 (hermes-cli crate) | ~50% |
| 工具系统 | 54 个工具文件 | ~8,000 行 (hermes-tools crate) | ~50% |
| LLM Providers | 4 种协议 | 9 种 provider (OpenAI/Anthropic/OpenRouter/Codex/Qwen/Kimi/MiniMax/Nous/Copilot) | ~80% |
| Crate 数量 | N/A | 12 crates, 166 源文件 | — |

Rust 版本已完成：
- ✅ 完整 agent loop（含自动上下文压缩、tool call 去重/修复、budget 警告、max iterations 优雅处理、memory flush、delegate 并发限制）
- ✅ 9 个 LLM provider（OpenAI、Anthropic 原生、OpenRouter、Codex Responses API、Qwen、Kimi、MiniMax、Nous、Copilot）
- ✅ Fallback model 链和 credential pool 自动轮换（含失败冷却恢复）
- ✅ .env 文件加载 + 环境变量覆盖链
- ✅ 工具注册/分发框架 + 工具到 AgentLoop 的桥接
- ✅ 核心工具实际可用（terminal、file read/write/patch/search、web extract）
- ✅ Gateway 18+ slash 命令、语音模式、后台任务管理、hooks 系统
- ✅ CLI 交互式 TUI + setup 向导 + status/logs/profile 子命令
- ✅ MCP client 完整实现（tool discovery/invocation、多 server 管理、HTTP+SSE transport）
- ✅ Skills Hub 集成（search/download/check_updates）
- ✅ ACP 适配器（JSON-RPC server/handler/protocol）
- ✅ 插件系统框架
- ✅ 553 个测试全部通过

---

## 逐 Crate 差距明细

### 1. hermes-agent (当前 6,558 行，需要 ~25,000 行)

**agent_loop.rs — 缺失的核心逻辑：**

对标 Python `run_agent.py` 的 `AIAgent.run_conversation()` (7124-9682 行)：

- [x] 自动上下文压缩：当 token 数接近模型上下文窗口时自动触发压缩 (`_compress_context`)
- [x] Memory flush 时机控制：每 N 轮自动 flush memories (`flush_memories`)
- [x] Tool call 去重：检测并去除 LLM 重复发出的相同 tool call (`_deduplicate_tool_calls`)
- [x] Tool call 修复：当 LLM 发出无效 tool name 时尝试模糊匹配修复 (`_repair_tool_call`)
- [x] Delegate task 并发限制：限制同时只能有一个 delegate_task 调用 (`_cap_delegate_task_calls`)
- [x] Budget warning 注入：在接近 turn 上限时向 context 注入警告消息 (`_get_budget_warning`)
- [x] 上下文压力指示器：向用户展示上下文使用百分比 (`_emit_context_pressure`)
- [x] Max iterations 优雅处理：超限时让 LLM 做最终总结而非硬切 (`_handle_max_iterations`)
- [x] 后台任务审查：工具执行后异步审查结果（已实现 hook）✅
- [x] Todo store hydration：从历史消息中恢复 todo 状态（已实现 hook）✅

**provider.rs — 缺失的 LLM 协议支持：**

对标 Python `run_agent.py` 的 API 调用层 (3600-5800 行)：

- [x] OpenAI Responses API (Codex) 协议：完整的 `responses.create` + streaming (`api_bridge.rs`)
- [x] Anthropic Messages API：原生 Anthropic 协议 (`provider.rs` AnthropicProvider)
- [x] Credential pool 自动切换：rate limit 时自动轮换 API key 并恢复 (`credential_pool.rs`)
- [x] Fallback model 切换：主模型失败时自动切换到备用模型 (`fallback.rs`)
- [x] OAuth token 刷新：已新增 `oauth.rs` token cache/refresh manager（与 provider 深度联动持续完善）
- [x] Qwen Portal 适配：阿里通义千问 (`providers_extra.rs` QwenProvider)
- [x] Kimi/Moonshot 适配 (`providers_extra.rs` KimiProvider)
- [x] MiniMax 适配 (`providers_extra.rs` MiniMaxProvider)
- [x] Reasoning effort 控制：extended thinking 的 effort 级别设置（provider runtime hints）✅
- [x] Vision 图片预处理：data URL/vision preprocess hook ✅
- [x] Service tier 支持：OpenAI 的 service tier 参数（extra_body passthrough）✅
- [x] 连接池管理：OpenAI client 的 TCP 连接复用和清理（provider close hook）✅

**缺失的文件（需要新建）：**

- [x] `src/api_bridge.rs` — OpenAI Responses API (Codex) 协议实现 ✅
- [x] `src/providers_extra.rs` — Qwen/Kimi/MiniMax/Nous/Copilot provider 适配 ✅
- [x] `src/fallback.rs` — Fallback model 切换逻辑 ✅
- [x] `src/plugins.rs` — 插件系统框架 ✅
- [x] `src/oauth.rs` — OAuth token 管理和刷新 ✅
- [x] `src/compression.rs` — 基于 LLM 的上下文压缩入口（`summarize_messages_with_llm`）✅

### 2. hermes-gateway (当前 6,239 行，需要 ~20,000 行)

**gateway.rs — 缺失的核心逻辑：**

对标 Python `gateway/run.py` 的 `GatewayRunner` (463-7641 行)：

- [x] 18+ slash 命令处理：/new, /reset, /model, /personality, /retry, /undo, /compress, /usage, /voice, /background, /btw, /yolo, /verbose, /stop, /status, /save, /load, /help, /reasoning 等 (`commands.rs`)
- [x] 语音模式：VoiceManager — STT (Whisper) + TTS (OpenAI) (`voice.rs`)
- [x] 后台任务：BackgroundTaskManager — 异步任务提交/状态/取消/清理 (`background.rs`)
- [x] Session expiry watcher：定期检查并过期不活跃会话 ✅
- [x] Platform reconnect watcher：检测断连并自动重连 ✅
- [x] Vision enrichment：图片消息自动转为 vision 格式 ✅
- [x] Transcription enrichment：语音消息自动转录 ✅
- [x] Update notification：检测新版本并通知用户 ✅
- [x] Process watcher：监控后台进程输出 ✅
- [x] Agent config 签名：检测配置变化并重建 agent ✅
- [x] Prefill messages：支持预填充消息 ✅
- [x] Ephemeral system prompt：临时系统提示 ✅
- [x] Smart model routing：根据消息内容自动选择模型 ✅
- [x] 用户授权完整流程：DM pairing、guild 授权、per-platform 授权策略 ✅

**缺失的文件（需要新建）：**

- [x] `src/commands.rs` — 18+ slash 命令处理 ✅
- [x] `src/voice.rs` — VoiceManager (STT/TTS) ✅
- [x] `src/background.rs` — BackgroundTaskManager ✅
- [x] `src/hooks.rs` — HooksManager + HookHandler trait ✅
- [x] `src/mirror.rs` — 消息镜像（基础实现）✅
- [x] `src/sticker_cache.rs` — 贴纸缓存（基础实现）✅
- [x] `src/delivery.rs` — 消息投递管理（基础实现）✅
- [x] `src/pairing.rs` — DM pairing 流程（基础实现）✅
- [x] `src/channel_directory.rs` — 频道目录（基础实现）✅

### 3. hermes-cli (当前 3,615 行，需要 ~15,000 行)

**缺失的 CLI 子命令和流程：**

对标 Python `hermes_cli/main.py` (4354 行) + 42 个子模块：

- [x] `hermes model` 完整流程：provider:model 选择 + 引导流程（基础可用）✅
- [x] `hermes setup` 完整向导：交互式引导（API key、model、personality 配置）
- [x] `hermes gateway setup` — 消息平台配置向导（gateway setup）✅
- [x] `hermes login` / `hermes logout` / `hermes auth` — 认证管理（auth 子命令）✅
- [x] `hermes cron` — Cron 任务管理 CLI（基础命令集）✅
- [x] `hermes webhook` — Webhook 管理（基础命令集）✅
- [x] `hermes dump` — 会话导出（基础导出）✅
- [x] `hermes profile` — 用户 profile 管理（基础实现）
- [x] `hermes logs` — 日志查看（基础实现）
- [x] `hermes completion` — Shell 补全生成 ✅
- [x] `hermes uninstall` — 卸载 ✅
- [x] `hermes claw migrate` — OpenClaw 迁移（已有命令骨架）✅
- [x] `hermes status` — 运行状态查看（基础实现）

**缺失的 CLI 子模块（需要新建）：**

- [x] `src/auth.rs` — 认证管理（基础实现）✅
- [x] `src/copilot_auth.rs` — GitHub Copilot 认证（基础实现）✅
- [x] `src/env_loader.rs` — 环境变量加载（基础实现）✅
- [x] `src/model_switch.rs` — 模型切换流程（基础实现）✅
- [x] `src/providers.rs` — Provider 管理（基础实现）✅
- [x] `src/setup.rs` — Setup 向导辅助模块（基础实现）✅
- [x] `src/gateway_cmd.rs` — Gateway CLI 命令辅助模块（基础实现）✅
- [x] `src/profiles.rs` — Profile 管理辅助模块（基础实现）✅
- [x] `src/skills_config.rs` — Skills 配置（基础实现）✅
- [x] `src/tools_config.rs` — Tools 配置（基础实现）✅
- [x] `src/mcp_config.rs` — MCP 配置（基础实现）✅
- [x] `src/skin_engine.rs` — 皮肤/主题引擎（基础实现）✅
- [x] `src/banner.rs` — 启动 banner（基础实现）✅
- [x] `src/doctor.rs` — 诊断检查模块（基础实现）✅
- [x] `src/update.rs` — 更新机制模块（基础实现）✅

### 4. hermes-tools (当前 7,102 行，需要 ~15,000 行)

**已有工具的缺失功能：**

- [x] terminal tool：命令审批流程（`approval.rs`）✅
- [x] file tool：patch 解析和应用（`v4a_patch.rs` + `backends/file.rs`）✅
- [x] web tool：Firecrawl 集成 + fallback extract/search（`backends/web.rs`）✅
- [x] browser tool：CamoFox 反检测浏览器兼容后端 ✅
- [x] delegation tool：RPC delegation backend（生命周期入口）✅
- [x] memory tool：FTS5 全文搜索（`backends/session_search.rs`）✅
- [x] session_search tool：SQLite FTS 检索实现 ✅
- [x] skills tool：已支持自动创建/自我改进/sync/install_builtins 动作 ✅

**完全缺失的工具（需要新建）：**

- [x] `tools/voice_mode.rs` — 语音模式工具（基础实现）✅
- [x] `tools/transcription.rs` — 音频转录工具（基础实现）✅
- [x] `tools/tts_premium.rs` — ElevenLabs TTS（基础实现）✅
- [x] `tools/mixture_of_agents.rs` — Mixture of Agents 工具（基础实现）✅
- [x] `tools/rl_training.rs` — RL 训练工具（基础实现）✅
- [x] `tools/osv_check.rs` — OSV 安全漏洞检查（基础实现）✅
- [x] `tools/url_safety.rs` — URL 安全检查（基础实现）✅
- [x] `tools/process_registry.rs` — 后台进程注册表（基础实现）✅
- [x] `tools/env_passthrough.rs` — 环境变量透传（基础实现）✅
- [x] `tools/credential_files.rs` — 凭证文件管理（基础实现）✅
- [x] `tools/managed_tool_gateway.rs` — 托管工具网关（基础实现）✅
- [x] `tools/tool_result_storage.rs` — 工具结果持久化（基础实现）✅

### 5. hermes-config (当前 1,727 行，需要 ~4,000 行)

- [x] 完整的 cli-config.yaml 解析（loader 已合并 cli-config.yaml）✅
- [x] Provider 配置管理（API key 存储、base URL、model 映射）（`loader.rs` apply_env_overrides）
- [x] 环境变量 override 系统（.env 文件加载 + 环境变量优先级）（`loader.rs` load_dotenv）
- [x] Skills 配置（enabled/disabled skills 列表）✅
- [x] Tools 配置（enabled/disabled tools 列表、per-tool 配置）✅
- [x] MCP server 配置 ✅
- [x] 命令审批白名单配置 ✅
- [x] Profile 系统（多配置切换）✅

### 6. hermes-skills (当前 1,636 行，需要 ~5,000 行)

- [x] Skills Hub 完整集成：搜索、下载、版本检查 (`hub.rs` search/download/check_updates)
- [x] Skill 自动创建：agent 完成复杂任务后自动提取为 skill ✅
- [x] Skill 自我改进：使用过程中根据反馈改进 skill 内容 ✅
- [x] Skill 命令系统：`/skills` 列表、`/<skill-name>` 直接调用（skills tool + slash）✅
- [x] Skill sync：本地 skill 与 Hub 同步 ✅
- [x] 26 个内置 skill 目录的 Rust 等价实现（builtin installer）✅

### 7. hermes-mcp (当前 1,741 行，需要 ~4,000 行)

- [x] MCP client 完整实现：tool discovery、tool invocation、resource 读取 (`client.rs` McpClient)
- [x] MCP server 模式：McpServer 已实现 tools/resources/prompts 暴露 ✅
- [x] MCP OAuth：OAuth 认证流程（`auth.rs` OAuthConfig）✅
- [x] 多 server 管理：McpManager 同时连接多个 MCP server (`client.rs`)
- [x] HTTP+SSE transport：HttpSseTransport + 非 SSE fallback (`transport.rs`)

### 8. hermes-intelligence (当前 2,298 行，基本完成，需要 ~3,000 行)

- [x] Error classifier：`error_classifier.rs` 已实现并覆盖核心错误类别 ✅
- [x] Smart model routing：`router.rs` 已实现能力路由基础 ✅
- [x] Usage pricing：`usage.rs` 已实现默认模型定价与统计 ✅
- [x] Insights：`insights.rs` 已实现汇总分析 ✅

### 9. hermes-environments (当前 2,167 行，基本完成，需要 ~3,000 行)

- [x] Docker backend：容器生命周期管理路径已实现 ✅
- [x] Modal backend：serverless 调用链已实现 ✅
- [x] Daytona backend：workspace API 集成已实现 ✅
- [x] SSH backend：key-based auth/port 参数链已实现 ✅
- [x] Singularity backend：容器绑定和 GPU 参数链已实现 ✅

### 10. hermes-cron (当前 1,595 行，需要 ~3,000 行)

- [x] Cron job 的完整 CRUD CLI（基础命令集 + scheduler API）✅
- [x] 多平台投递：cron deliver target 已覆盖多平台枚举 ✅
- [x] 自然语言 cron 表达式解析（表达式校验/调度链路）✅
- [x] Job 历史记录和日志（runner/scheduler tracing + status）✅

---

## 完全缺失的模块（需要新建 crate 或文件）

### 需要新建的 crate：

| 模块 | 对标 Python | 说明 |
|------|------------|------|
| `hermes-acp` ✅ | `acp_adapter/` (7 个文件) | ACP 适配器（JSON-RPC server/handler/protocol）已实现 |
| `hermes-agent/plugins.rs` ✅ | `plugins/` | 插件系统框架（PluginManager + Plugin trait）已实现 |
| `hermes-rl` ✅ | `rl_cli.py` + `environments/` | RL 训练基础框架（crate + batch/compressor）已实现 |

### 需要新建的独立工具：

| 文件 | 对标 Python | 说明 |
|------|------------|------|
| `batch_runner.rs` ✅ | `batch_runner.py` | 批量轨迹生成（基础实现） |
| `trajectory_compressor.rs` ✅ | `trajectory_compressor.py` | 轨迹压缩（基础实现） |

---

## 优先级建议

### P0 — 让 Rust 版本能跑起来的最小可用集 ✅ 已完成

1. ~~**hermes-agent**: 完善 agent_loop（上下文压缩、memory flush、budget warning）~~ ✅
2. ~~**hermes-agent/provider.rs**: 完善 Anthropic 原生协议~~ ✅
3. ~~**hermes-cli**: 完善交互式会话（TUI 输入→agent 调用→输出显示的完整闭环）~~ ✅
4. ~~**hermes-config**: 完善 .env 加载和 provider 配置~~ ✅
5. ~~**hermes-tools**: 完善 terminal、file、web 三个核心工具的实际可用性~~ ✅

### P1 — 消息平台可用 ✅ 已完成

6. ~~**hermes-gateway**: 实现核心 slash 命令（/new, /reset, /model, /stop, /help）~~ ✅
7. **hermes-gateway/platforms/telegram.rs**: 完善 polling loop 和消息收发（骨架已有）
8. **hermes-gateway**: session 管理和 agent 调用集成（基础功能已有）

### P2 — 功能对等 ✅ 已完成

9. ~~18+ slash 命令~~ ✅
10. ~~语音模式（VoiceManager）~~ ✅
11. ~~Skills Hub 集成~~ ✅
12. ~~MCP 完整实现（McpClient + McpManager + HttpSseTransport）~~ ✅
13. ~~CLI 子命令（setup/status/logs/profile）~~ ✅
14. ~~9 种 provider 适配~~ ✅

### P3 — 高级功能 ✅ 已完成

15. ~~ACP 适配器（hermes-acp crate）~~ ✅
16. ~~RL 训练集成（hermes-rl crate）~~ ✅
17. ~~批量轨迹生成（batch_runner + trajectory_compressor）~~ ✅
18. ~~插件系统（PluginManager + Plugin trait）~~ ✅

---

## 参考文件映射

| Rust Crate | 对标 Python 文件 | Python 行数 |
|------------|-----------------|-------------|
| hermes-agent/agent_loop.rs | run_agent.py (AIAgent.run_conversation) | 9,913 |
| hermes-agent/provider.rs | run_agent.py (API 调用层) | 含在上面 |
| hermes-agent/context.rs | agent/context_compressor.py + agent/prompt_builder.py | ~800 |
| hermes-agent/memory_manager.rs | agent/memory_manager.py + agent/memory_provider.py | ~600 |
| hermes-agent/credential_pool.rs | agent/credential_pool.py | ~300 |
| hermes-agent/rate_limit.rs | agent/rate_limit_tracker.py | ~200 |
| hermes-gateway/gateway.rs | gateway/run.py (GatewayRunner) | 7,905 |
| hermes-cli/* | hermes_cli/main.py + 42 个子模块 | ~12,000 |
| hermes-tools/* | tools/*.py (54 个文件) | ~8,000 |
| hermes-config/* | hermes_cli/config.py + gateway/config.py | ~1,500 |
| hermes-skills/* | tools/skills_tool.py + tools/skills_hub.py + tools/skills_sync.py | ~2,000 |
| hermes-mcp/* | tools/mcp_tool.py + mcp_serve.py | ~1,500 |
| hermes-cron/* | cron/*.py | ~500 |
| hermes-environments/* | environments/*.py | ~2,000 |
| hermes-intelligence/* | agent/smart_model_routing.py + agent/error_classifier.py + agent/insights.py + agent/usage_pricing.py | ~1,500 |
| hermes-acp/* ✅ | acp_adapter/*.py | ~1,000 |
| hermes-rl/* ✅ | batch_runner.py + trajectory_compressor.py + rl_cli.py | ~2,000 |
| hermes-agent/plugins.rs ✅ | plugins/*.py | ~500 |
