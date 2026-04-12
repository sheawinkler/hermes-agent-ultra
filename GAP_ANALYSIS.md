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

- [ ] 自动上下文压缩：当 token 数接近模型上下文窗口时自动触发压缩 (`_compress_context`)
- [ ] Memory flush 时机控制：每 N 轮自动 flush memories (`flush_memories`)
- [ ] Tool call 去重：检测并去除 LLM 重复发出的相同 tool call (`_deduplicate_tool_calls`)
- [ ] Tool call 修复：当 LLM 发出无效 tool name 时尝试模糊匹配修复 (`_repair_tool_call`)
- [ ] Delegate task 并发限制：限制同时只能有一个 delegate_task 调用 (`_cap_delegate_task_calls`)
- [ ] Budget warning 注入：在接近 turn 上限时向 context 注入警告消息 (`_get_budget_warning`)
- [ ] 上下文压力指示器：向用户展示上下文使用百分比 (`_emit_context_pressure`)
- [ ] Max iterations 优雅处理：超限时让 LLM 做最终总结而非硬切 (`_handle_max_iterations`)
- [ ] 后台任务审查：工具执行后异步审查结果 (`_spawn_background_review`)
- [ ] Todo store hydration：从历史消息中恢复 todo 状态 (`_hydrate_todo_store`)

**provider.rs — 缺失的 LLM 协议支持：**

对标 Python `run_agent.py` 的 API 调用层 (3600-5800 行)：

- [ ] OpenAI Responses API (Codex) 协议：完整的 `responses.create` + streaming (`_run_codex_stream`)
- [ ] Anthropic Messages API：原生 Anthropic 协议而非 OpenAI 兼容 (`_anthropic_messages_create`)
- [ ] Credential pool 自动切换：rate limit 时自动轮换 API key 并恢复 (`_recover_with_credential_pool`)
- [ ] Fallback model 切换：主模型失败时自动切换到备用模型 (`_try_activate_fallback`, `_restore_primary_runtime`)
- [ ] OAuth token 刷新：Codex/Nous/Anthropic 的 OAuth credential 自动刷新
- [ ] Qwen Portal 适配：阿里通义千问的特殊消息格式处理 (`_qwen_prepare_chat_messages`)
- [ ] Kimi/Moonshot 适配
- [ ] MiniMax 适配
- [ ] Reasoning effort 控制：extended thinking 的 effort 级别设置
- [ ] Vision 图片预处理：data URL 物化、Anthropic 格式转换 (`_preprocess_anthropic_content`)
- [ ] Service tier 支持：OpenAI 的 service tier 参数
- [ ] 连接池管理：OpenAI client 的 TCP 连接复用和清理 (`_force_close_tcp_sockets`)

**缺失的文件（需要新建）：**

- [ ] `src/api_bridge.rs` — OpenAI Responses API (Codex) 协议实现
- [ ] `src/anthropic_native.rs` — 原生 Anthropic Messages API（当前 provider.rs 里的 AnthropicProvider 是通过 OpenAI 兼容层，需要原生实现）
- [ ] `src/fallback.rs` — Fallback model 切换逻辑
- [ ] `src/oauth.rs` — OAuth token 管理和刷新
- [ ] `src/compression.rs` — 基于 LLM 的上下文压缩（当前 context.rs 里的 ContextCompressor 是简单截断，Python 版本用 LLM 做摘要）

### 2. hermes-gateway (当前 6,239 行，需要 ~20,000 行)

**gateway.rs — 缺失的核心逻辑：**

对标 Python `gateway/run.py` 的 `GatewayRunner` (463-7641 行)：

- [ ] 50+ slash 命令处理：/new, /reset, /model, /personality, /retry, /undo, /compress, /usage, /insights, /voice, /background, /btw, /reasoning, /fast, /yolo, /verbose, /title, /resume, /branch, /rollback, /approve, /deny, /update, /reload_mcp, /sethome, /status, /help, /commands, /provider, /profile 等
- [ ] 语音模式：语音消息转录、TTS 回复、Discord 语音频道加入/离开 (`_handle_voice_command`)
- [ ] 后台任务：/background 和 /btw 命令触发的异步任务 (`_run_background_task`, `_run_btw_task`)
- [ ] Session expiry watcher：定期检查并过期不活跃会话 (`_session_expiry_watcher`)
- [ ] Platform reconnect watcher：检测断连并自动重连 (`_platform_reconnect_watcher`)
- [ ] Vision enrichment：图片消息自动转为 vision 格式 (`_enrich_message_with_vision`)
- [ ] Transcription enrichment：语音消息自动转录 (`_enrich_message_with_transcription`)
- [ ] Update notification：检测新版本并通知用户 (`_send_update_notification`)
- [ ] Process watcher：监控后台进程输出 (`_run_process_watcher`)
- [ ] Agent config 签名：检测配置变化并重建 agent (`_agent_config_signature`)
- [ ] Prefill messages：支持预填充消息 (`_load_prefill_messages`)
- [ ] Ephemeral system prompt：临时系统提示 (`_load_ephemeral_system_prompt`)
- [ ] Smart model routing：根据消息内容自动选择模型 (`_load_smart_model_routing`)
- [ ] 用户授权完整流程：DM pairing、guild 授权、per-platform 授权策略 (`_is_user_authorized`)

**缺失的文件（需要新建）：**

- [ ] `src/commands.rs` — 所有 slash 命令的处理逻辑
- [ ] `src/voice.rs` — 语音模式管理
- [ ] `src/background.rs` — 后台任务管理
- [ ] `src/hooks.rs` — Gateway hooks 系统（对标 `gateway/hooks.py`）
- [ ] `src/mirror.rs` — 消息镜像（对标 `gateway/mirror.py`）
- [ ] `src/sticker_cache.rs` — 贴纸缓存（对标 `gateway/sticker_cache.py`）
- [ ] `src/delivery.rs` — 消息投递管理（对标 `gateway/delivery.py`）
- [ ] `src/pairing.rs` — DM pairing 流程（对标 `gateway/pairing.py`）
- [ ] `src/channel_directory.rs` — 频道目录（对标 `gateway/channel_directory.py`）

### 3. hermes-cli (当前 3,615 行，需要 ~15,000 行)

**缺失的 CLI 子命令和流程：**

对标 Python `hermes_cli/main.py` (4354 行) + 42 个子模块：

- [ ] `hermes model` 完整流程：10+ provider 各自的交互式选择流程（OpenRouter 模型搜索、Nous Portal OAuth、OpenAI Codex OAuth、Anthropic OAuth、Copilot ACP、Kimi、Lumio、自定义 provider 管理等）
- [ ] `hermes setup` 完整向导：首次配置的交互式引导
- [ ] `hermes gateway setup` — 消息平台配置向导
- [ ] `hermes login` / `hermes logout` / `hermes auth` — 认证管理
- [ ] `hermes cron` — Cron 任务管理 CLI
- [ ] `hermes webhook` — Webhook 管理
- [ ] `hermes dump` — 会话导出
- [ ] `hermes profile` — 用户 profile 管理（session 浏览、重命名、删除、导出）
- [ ] `hermes logs` — 日志查看
- [ ] `hermes completion` — Shell 补全生成
- [ ] `hermes uninstall` — 卸载
- [ ] `hermes claw migrate` — OpenClaw 迁移（Rust 有骨架但不完整）
- [ ] `hermes status` — 运行状态查看

**缺失的 CLI 子模块（需要新建）：**

- [ ] `src/auth.rs` — 认证管理（对标 `hermes_cli/auth.py` + `auth_commands.py`）
- [ ] `src/copilot_auth.rs` — GitHub Copilot 认证（对标 `hermes_cli/copilot_auth.py`）
- [ ] `src/env_loader.rs` — 环境变量加载（对标 `hermes_cli/env_loader.py`）
- [ ] `src/model_switch.rs` — 模型切换完整流程（对标 `hermes_cli/model_switch.py`）
- [ ] `src/providers.rs` — Provider 管理（对标 `hermes_cli/providers.py`）
- [ ] `src/setup.rs` — Setup 向导（对标 `hermes_cli/setup.py`）
- [ ] `src/gateway_cmd.rs` — Gateway CLI 命令（对标 `hermes_cli/gateway.py`）
- [ ] `src/profiles.rs` — Profile 管理（对标 `hermes_cli/profiles.py`）
- [ ] `src/skills_config.rs` — Skills 配置（对标 `hermes_cli/skills_config.py`）
- [ ] `src/tools_config.rs` — Tools 配置（对标 `hermes_cli/tools_config.py`）
- [ ] `src/mcp_config.rs` — MCP 配置（对标 `hermes_cli/mcp_config.py`）
- [ ] `src/skin_engine.rs` — 皮肤/主题引擎（对标 `hermes_cli/skin_engine.py`）
- [ ] `src/banner.rs` — 启动 banner（对标 `hermes_cli/banner.py`）
- [ ] `src/doctor.rs` — 诊断检查完整实现（对标 `hermes_cli/doctor.py`）
- [ ] `src/update.rs` — 更新机制（git + zip 双路径）

### 4. hermes-tools (当前 7,102 行，需要 ~15,000 行)

**已有工具的缺失功能：**

- [ ] terminal tool：缺少命令审批流程 (`tools/approval.py` 的完整实现)
- [ ] file tool：缺少 patch 解析和应用 (`tools/patch_parser.py`)
- [ ] web tool：缺少 parallel-web 并发抓取、Firecrawl 集成
- [ ] browser tool：缺少 CamoFox 反检测浏览器 (`tools/browser_camofox.py`)
- [ ] delegation tool：缺少子 agent 的完整生命周期管理 (`tools/delegate_tool.py` 的 RPC 模式)
- [ ] memory tool：缺少 FTS5 全文搜索 (`tools/memory_tool.py` 的 SQLite FTS)
- [ ] session_search tool：缺少 LLM 摘要搜索 (`tools/session_search_tool.py`)
- [ ] skills tool：缺少 skill 自动创建和自我改进 (`tools/skills_tool.py`)

**完全缺失的工具（需要新建）：**

- [ ] `tools/voice_mode.rs` — 语音模式工具（STT/TTS 切换）
- [ ] `tools/transcription.rs` — 音频转录工具
- [ ] `tools/tts_premium.rs` — ElevenLabs TTS
- [ ] `tools/mixture_of_agents.rs` — Mixture of Agents 工具
- [ ] `tools/rl_training.rs` — RL 训练工具
- [ ] `tools/osv_check.rs` — OSV 安全漏洞检查
- [ ] `tools/url_safety.rs` — URL 安全检查
- [ ] `tools/process_registry.rs` — 后台进程注册表
- [ ] `tools/env_passthrough.rs` — 环境变量透传
- [ ] `tools/credential_files.rs` — 凭证文件管理
- [ ] `tools/managed_tool_gateway.rs` — 托管工具网关
- [ ] `tools/tool_result_storage.rs` — 工具结果持久化

### 5. hermes-config (当前 1,727 行，需要 ~4,000 行)

- [ ] 完整的 cli-config.yaml 解析（当前只有 gateway config）
- [ ] Provider 配置管理（API key 存储、base URL、model 映射）
- [ ] 环境变量 override 系统（.env 文件加载 + 环境变量优先级）
- [ ] Skills 配置（enabled/disabled skills 列表）
- [ ] Tools 配置（enabled/disabled tools 列表、per-tool 配置）
- [ ] MCP server 配置
- [ ] 命令审批白名单配置
- [ ] Profile 系统（多配置切换）

### 6. hermes-skills (当前 1,636 行，需要 ~5,000 行)

- [ ] Skills Hub 完整集成：搜索、下载、上传、版本检查（`hub.rs` 只有结构体定义）
- [ ] Skill 自动创建：agent 完成复杂任务后自动提取为 skill
- [ ] Skill 自我改进：使用过程中根据反馈改进 skill 内容
- [ ] Skill 命令系统：`/skills` 列表、`/<skill-name>` 直接调用
- [ ] Skill sync：本地 skill 与 Hub 同步 (`tools/skills_sync.py`)
- [ ] 26 个内置 skill 目录的 Rust 等价实现

### 7. hermes-mcp (当前 1,741 行，需要 ~4,000 行)

- [ ] MCP client 完整实现：tool discovery、tool invocation、resource 读取
- [ ] MCP server 模式：将 Hermes 自身暴露为 MCP server (`mcp_serve.py`)
- [ ] MCP OAuth：OAuth 认证流程 (`tools/mcp_oauth.py`)
- [ ] 多 server 管理：同时连接多个 MCP server
- [ ] Server 生命周期管理：启动、停止、重连

### 8. hermes-intelligence (当前 2,298 行，基本完成，需要 ~3,000 行)

- [ ] Error classifier：需要更多错误模式识别（rate limit、auth、context length 等）
- [ ] Smart model routing：需要完整的模型能力数据库和路由策略
- [ ] Usage pricing：需要完整的模型定价数据
- [ ] Insights：需要完整的使用统计分析

### 9. hermes-environments (当前 2,167 行，基本完成，需要 ~3,000 行)

- [ ] Docker backend：需要完整的容器生命周期管理（创建、启动、停止、删除）
- [ ] Modal backend：需要完整的 serverless 函数调用
- [ ] Daytona backend：需要完整的 workspace API 集成
- [ ] SSH backend：需要 key-based auth、port forwarding
- [ ] Singularity backend：需要完整的容器绑定和 GPU 支持

### 10. hermes-cron (当前 1,595 行，需要 ~3,000 行)

- [ ] Cron job 的完整 CRUD CLI
- [ ] 多平台投递：将 cron 结果投递到 Telegram/Discord/Slack 等
- [ ] 自然语言 cron 表达式解析
- [ ] Job 历史记录和日志

---

## 完全缺失的模块（需要新建 crate 或文件）

### 需要新建的 crate：

| 模块 | 对标 Python | 说明 |
|------|------------|------|
| `hermes-acp` | `acp_adapter/` (7 个文件) | Agent Communication Protocol 适配器 |
| `hermes-plugins` | `plugins/` | 插件系统框架 |
| `hermes-rl` | `rl_cli.py` + `environments/` | RL 训练集成 (Atropos/Tinker) |

### 需要新建的独立工具：

| 文件 | 对标 Python | 说明 |
|------|------------|------|
| `batch_runner.rs` | `batch_runner.py` | 批量轨迹生成 |
| `trajectory_compressor.rs` | `trajectory_compressor.py` | 轨迹压缩 |

---

## 优先级建议

### P0 — 让 Rust 版本能跑起来的最小可用集

1. **hermes-agent**: 完善 agent_loop（上下文压缩、memory flush、budget warning）
2. **hermes-agent/provider.rs**: 完善 Anthropic 原生协议
3. **hermes-cli**: 完善交互式会话（TUI 输入→agent 调用→输出显示的完整闭环）
4. **hermes-config**: 完善 .env 加载和 provider 配置
5. **hermes-tools**: 完善 terminal、file、web 三个核心工具的实际可用性

### P1 — 消息平台可用

6. **hermes-gateway**: 实现核心 slash 命令（/new, /reset, /model, /stop, /help）
7. **hermes-gateway/platforms/telegram.rs**: 完善 polling loop 和消息收发
8. **hermes-gateway**: session 管理和 agent 调用集成

### P2 — 功能对等

9. 所有 slash 命令
10. 语音模式
11. Skills Hub 集成
12. MCP 完整实现
13. 所有 CLI 子命令
14. 所有 provider 适配

### P3 — 高级功能

15. ACP 适配器
16. RL 训练集成
17. 批量轨迹生成
18. 插件系统

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
| (缺失) | acp_adapter/*.py | ~1,000 |
| (缺失) | batch_runner.py + trajectory_compressor.py + rl_cli.py | ~2,000 |
| (缺失) | plugins/*.py | ~500 |
