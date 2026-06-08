# SOP: `run_conversation`

| 字段 | 值 |
|------|-----|
| registry `id` | `run_conversation` |
| Python @ 1335ce | `run_agent.py::AIAgent.run_conversation` |
| Rust | `hermes_agent::conversation_loop` + `AgentLoop::run_prepared` / `run_stream_prepared` |
| Crate | `hermes-agent` |
| Contract tests | `crates/hermes-agent/tests/run_conversation_*.rs` |
| Fixtures | 可选 `fixtures/conversation_loop/`（不阻塞 merge） |

## 语义对齐原则

- **Adopt**：Python 用户/插件可感知行为更完整 → Rust 同等语义（见下表）。
- **Document**：Rust 结构或等价路径更优 → 登记 `docs/parity/intentional-divergence.json`，不字面 port。

## 必须语义对齐（Adopt）

| 行为 | Rust 位置 | 验证 |
|------|-----------|------|
| Turn 前奏：sanitize、@file、restore primary | `prepare_turn` / `apply_turn_message_prelude` | `cargo test -p hermes-agent preprocess_user_message` / `restore_primary` |
| Hooks：`on_session_end`、`pre_api_request` | `session_end_hooks` / `invoke_pre_api_request_hook` | `cargo test -p hermes-agent --test run_conversation_hooks` |
| Steer pre-API drain | `steer.rs` | `cargo test -p hermes-agent steer` |
| Truncated tool-call retry | `agent_loop.rs` | `run_truncated_tool_call_retries` |
| 编排 API | `run_conversation` | `cargo test -p hermes-agent --test run_conversation_contracts` |
| Gateway/HTTP 主路径 | `hermes-cli/main.rs`, `hermes-http` | 集成；`task_id` = `session_key` |

## Rust 优势（Document — 非欠账）

| 项 | 收益 |
|----|------|
| `conversation_loop` + `run_prepared` | 可测 B/E；`skip_message_prelude` 避免双重 prelude |
| Gateway 在 cli/http 装配 | 平台 crate 与 loop 解耦 |
| 无 Python runtime vendoring | 单二进制、`cargo test` 主门禁 |

完整 divergence id 见 `intentional-divergence.json` 中 `run-conversation-*` 条目。

## 验证（每 PR）

```bash
cargo build -p hermes-agent
cargo test -p hermes-agent --test run_conversation_hooks --test run_conversation_contracts
cargo clippy -p hermes-agent -- -D warnings
```

## Phase A 契约映射（`message_sanitization.rs`）

| # | 场景 | 决策 | 契约测试 | Divergence id |
|---|------|------|----------|---------------|
| 1 | 新会话 + `on_session_start` | Adopt（对齐 `conversation_loop._restore_or_build_system_prompt`） | `phase_a1_new_session_fires_on_session_start` | — |
| 2 | 续会话 `stored_system_prompt` | Adopt | `phase_a2_continue_session_skips_on_session_start`, `hydrate_stored_system_prompt_roundtrip` | — |
| 3 | Budget 70% caution | Adopt（`total_turns` / `max_turns` 比率，与配置文档一致） | `phase_a3_budget_caution_injected_at_seventy_percent` | — |
| 4 | Budget 90% warning | Adopt | `phase_a4_budget_warning_injected_at_ninety_percent` | — |
| 5 | History strip `_budget_warning` / `[BUDGET` | Adopt | `phase_a5_strip_budget_plain_text_tail_matches_python_regex`, `strip_budget_tool_message_matches_python_fixture` | — |
| 6 | Preflight compress | Adopt | `preflight_compression_status_*`（`agent_loop` 模块内） | — |
| 7 | 空 LLM 重试 | Adopt | `phase_a7_empty_llm_retry_without_appending_empty_assistant` | — |
| 8 | Streaming + interrupt | Adopt | `phase_a8_stream_interrupt_forwards_deltas_and_stops` | — |
| 9 | Hooks llm/tool/session_start | Adopt | `phase_a9_run_invokes_pre_post_llm_and_tool_hooks` | — |
| 10 | `AgentResult` cost / interrupted | Adopt | `phase_a10_*` | — |
| 11 | `run_conversation` 流式 | Adopt | `phase_a11_run_conversation_stream_callback_receives_deltas` | `run-conversation-orchestration-split`（编排路径） |
| 12 | `on_session_end` / `pre_api_request` | Adopt | `run_natural_finish_invokes_on_session_end_and_pre_api_request` | — |
| 13 | Steer pre-API + `pending_steer` | Adopt | `phase_a13_steer_pre_api_injects_into_last_tool_during_run`, `run_conversation_drains_pending_steer_into_result` | — |

```bash
cargo test -p hermes-agent --test alignment_contracts --test run_agent_phase_a --test run_agent_hooks --test run_conversation_hooks --test run_conversation_contracts
```

## 参考

- [`crates/hermes-agent/src/conversation_loop.rs`](../../crates/hermes-agent/src/conversation_loop.rs)
- [`crates/hermes-agent/src/message_sanitization.rs`](../../crates/hermes-agent/src/message_sanitization.rs) Phase A 列表
