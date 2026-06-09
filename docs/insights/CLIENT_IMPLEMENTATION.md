# Insights Contribution — 客户端实现说明（v3）

> **服务端规格**：见 [SERVER_V3_DOMAIN_WORK_PACKAGE.md](./SERVER_V3_DOMAIN_WORK_PACKAGE.md)  
> **Consent**：`2026-06-15`  
> **本仓库**：`crates/hermes-insights` + `crates/hermes-agent/src/work_session/` + CLI

---

## 实现状态

- [x] 唯一上传类型：`domain_work_package`（Domain POI + Skill + Resolution）
- [x] Resolution Verdict Engine（`work_session/resolution.rs`）
- [x] Session-end 管道：POI ingest → work package → outbox → flush
- [x] Skill 会话绑定：`session_skills.json` + `skill_manage` 钩子
- [x] CLI：`hermes contribute *`

---

## 上传什么

每个 `domain_work_package` 包含：

| 块 | 内容 |
|----|------|
| `domain_poi` | 专业/业务问题（`domain_key`、`problem_statement_redacted`） |
| `resolution` | 本地解题判定（`verdict`、`evidence_tier`、`signal_codes`） |
| `skill` | 绑定的个性化 skill 模式（`pattern_id`、`redacted_body`） |
| `work_metrics` | turn/tool/skill 分桶指标 |

**门控**（默认）：

- `min_evidence_tier: C`
- `exclude_verdicts: [abandoned, indeterminate]`
- `require_skill_binding: true`
- `min_work_turns: 2`

---

## 配置

```yaml
insights:
  contribution:
    enabled: false
    endpoint: "https://ops.example.com/v1/insights/batch"
    auth_token: "..."
    on_session_end: true
    redacted_body: true
    min_evidence_tier: C
    require_skill_binding: true
    min_work_turns: 2
```

```bash
hermes contribute enable
hermes contribute status
hermes contribute flush
```

---

## 验证

```bash
cargo test -p hermes-insights
cargo build -p hermes-agent
cargo build -p hermes-cli
```
