# Insights Contribution — 服务端规格 v3（Domain Work Package）

> **客户端（Hermes Rust）**：`domain_work_package` 为**唯一**贡献类型。  
> **Consent 版本**：`2026-06-15`（与 `INSIGHTS_CONSENT_VERSION` 一致）  
> **替代文档**：本规格完全取代 v2 的 `interest_snapshot` + `skill_pattern` 双类型上传；旧 v2 行可保留只读，新 ingest **仅接受 v3**。  
> **关联**：[CLIENT_IMPLEMENTATION.md](./CLIENT_IMPLEMENTATION.md)、[OPS_UI.md](./OPS_UI.md)

---

## 0. 变更摘要

| 项 | v2（废弃 ingest） | v3（当前） |
|----|-------------------|------------|
| 贡献类型 | `interest_snapshot` + `skill_pattern` | **`domain_work_package`** |
| Consent | `2026-05-29` | **`2026-06-15`** |
| 数据单元 | POI 与 Skill 离散 | **Domain POI + Skill + Resolution 原子包** |
| 解题标记 | 无 | **`resolution.verdict` / `evidence_tier` / `signal_codes`** |
| 运营主键 | topic_key / pattern_id | **`domain_key` + `pattern_id` + resolution** |

---

## 1. MySQL 迁移

### 1.1 修改 `insights_raw_contributions`

```sql
-- migrations/002_insights_v3_domain_work_package.up.sql

ALTER TABLE insights_raw_contributions
  MODIFY contribution_type ENUM('domain_work_package') NOT NULL;

ALTER TABLE insights_raw_contributions
  ADD COLUMN domain_key VARCHAR(128) NULL AFTER pattern_id,
  ADD COLUMN resolution_verdict VARCHAR(32) NULL AFTER domain_key,
  ADD COLUMN resolution_confidence_band VARCHAR(16) NULL AFTER resolution_verdict,
  ADD COLUMN resolution_evidence_tier CHAR(1) NULL AFTER resolution_confidence_band,
  ADD COLUMN reportable_signals JSON NULL COMMENT 'signal_codes copy' AFTER resolution_evidence_tier;

CREATE INDEX idx_insights_raw_domain_verdict
  ON insights_raw_contributions (domain_key, resolution_verdict, resolution_evidence_tier);
```

> 若需保留历史 v2 行：先备份表，新环境可直接 DROP 重建；混合环境可将 `contribution_type` 扩为 ENUM 含旧值但 ingest 拒绝旧 type。

### 1.2 新表 `insights_resolution_facts`

```sql
CREATE TABLE insights_resolution_facts (
    id                      BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    content_hash            CHAR(64)        NOT NULL,
    batch_id                CHAR(36)        NOT NULL,
    installation_id         CHAR(36)        NOT NULL,
    work_id_hash            CHAR(64)        NOT NULL COMMENT 'sha256(work_id), non-reversible',
    domain_key              VARCHAR(128)    NOT NULL,
    taxonomy_code           VARCHAR(128)    NULL,
    problem_class           VARCHAR(32)     NOT NULL,
    verdict                 VARCHAR(32)     NOT NULL,
    confidence_band         VARCHAR(16)     NOT NULL,
    evidence_tier           CHAR(1)         NOT NULL,
    user_feedback_band      VARCHAR(32)     NOT NULL,
    objective_check_band    VARCHAR(32)     NULL,
    recovery_attempted      TINYINT(1)      NOT NULL DEFAULT 0,
    signal_codes            JSON            NOT NULL,
    turn_band               VARCHAR(16)     NULL,
    pattern_id              VARCHAR(64)     NULL,
    received_at             DATETIME(3)     NOT NULL,
    deleted_at              DATETIME(3)     NULL,
    UNIQUE KEY uk_resolution_content (content_hash),
    KEY idx_resolution_domain_verdict (domain_key, verdict, evidence_tier),
    KEY idx_resolution_installation (installation_id, received_at),
    CONSTRAINT fk_resolution_batch
        FOREIGN KEY (batch_id) REFERENCES insights_ingest_batches (batch_id),
    CONSTRAINT fk_resolution_installation
        FOREIGN KEY (installation_id) REFERENCES insights_installations (installation_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
```

### 1.3 聚合表（P1 运营）

```sql
CREATE TABLE insights_domain_skill_edges (
    domain_key              VARCHAR(128)    NOT NULL,
    pattern_cluster_id      VARCHAR(64)     NOT NULL,
    co_occurrence_count     INT UNSIGNED    NOT NULL DEFAULT 0,
    weighted_success_score  DOUBLE          NOT NULL DEFAULT 0,
    distinct_installations  INT UNSIGNED    NOT NULL DEFAULT 0,
    last_seen_at            DATETIME(3)     NOT NULL,
    PRIMARY KEY (domain_key, pattern_cluster_id),
    KEY idx_edges_success (weighted_success_score DESC)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
```

---

## 2. 环境变量

| 变量 | 默认 | 说明 |
|------|------|------|
| `INSIGHTS_CONSENT_VERSION` | `2026-06-15` | 不匹配 → HTTP 400 |
| `INSIGHTS_PAYLOAD_INLINE_MAX` | `4096` | 超限 payload 上 OSS |
| `INSIGHTS_K_ANONYMITY` | `5` | 聚合层最小 distinct installations |
| `INSIGHTS_MIN_EVIDENCE_TIER` | `B` | Standard Skill 候选池默认门槛 |

---

## 3. `POST /v1/insights/batch`

### 3.1 请求

Headers 不变：`Authorization: Bearer`、`X-Installation-Id`（必填）、可选 `X-Client-Version`。

```json
{
  "batch_id": "550e8400-e29b-41d4-a716-446655440000",
  "consent_version": "2026-06-15",
  "contributions": [
    {
      "type": "domain_work_package",
      "collected_at": "2026-06-15T10:00:00Z",
      "content_hash": "64位hex小写",
      "payload": { }
    }
  ]
}
```

### 3.2 响应（不变）

```json
{
  "accepted": 1,
  "duplicates": 0,
  "rejected": [{ "content_hash": "...", "reason": "pii_detected" }]
}
```

HTTP 语义与 v2 相同（401/409/422/200）。

### 3.3 Ingest 伪代码（v3）

```
HandleBatch(req):
  if consent_version != "2026-06-15" → 400

  for item in req.contributions:
    if item.type != "domain_work_package" → schema_violation

    if item.resolution.verdict == "indeterminate" → schema_violation

    if duplicate content_hash → duplicates++

    validateDomainWorkPackage(item.payload)  // §4
    piiScan(item.payload)

    persist raw row + resolution_facts row  // §5
    asyncUpdateDomainSkillEdge(item)        // §6

  return 200
```

---

## 4. Payload 校验 — `domain_work_package`

完整 JSON 见 §7。校验规则：

### 4.1 顶层

| 字段 | 规则 |
|------|------|
| `schema_version` | 必填，整数 `1` |
| `work_id` | 必填 UUID |
| `session_id_hash` | 必填 64 hex（sha256(session_id)） |
| `domain_poi` | 必填 object |
| `resolution` | 必填 object |
| `skill` | 必填 object |
| `work_metrics` | 必填 object |

### 4.2 `domain_poi`

| 字段 | 规则 |
|------|------|
| `domain_key` | 必填；`[a-z0-9][a-z0-9._-]{2,127}` |
| `problem_statement_redacted` | 必填非空；PII 扫描 |
| `problem_class` | `operational` \| `technical` \| `compliance` \| `creative` \| `research` |
| `difficulty_band` | `low` \| `med` \| `high` |
| `taxonomy_code` | 可选；若存在须匹配 `^[a-z0-9.]+$` |

### 4.3 `resolution`（解题标记 — 核心）

| 字段 | 规则 |
|------|------|
| `verdict` | 见 §4.4；**禁止** `indeterminate` |
| `confidence_band` | `high` \| `medium` \| `low` |
| `evidence_tier` | `A` \| `B` \| `C` \| `D` |
| `user_feedback_band` | `explicit_positive` \| `explicit_negative` \| `neutral` \| `unknown` |
| `objective_check_band` | 可选：`pass` \| `fail` \| `not_applicable` |
| `signal_codes` | 非空数组；每项须在 §4.5 白名单 |
| `recovery_attempted` | bool |

### 4.4 `verdict` 枚举

```
solved_confirmed
solved_inferred
partial
unresolved
failed
abandoned
```

### 4.5 `signal_codes` 白名单

```
user_explicit_positive
user_explicit_negative
user_correction_loop
closure_without_followup
followup_same_poi_later
objective_test_pass
objective_test_fail
objective_not_applicable
skill_created_this_session
skill_patched_this_session
insufficient_turns
```

### 4.6 `skill`

| 字段 | 规则 |
|------|------|
| `pattern_id` | 64 hex |
| `display_name` | 必填非空 |
| `name_slug` | `[a-z0-9-]+` |
| `binding_role` | `primary` \| `supporting` \| `recovery` |
| `domain_keys` | 非空；须含 `domain_poi.domain_key` |
| `structure` | 同 v2 |
| `tool_chain` | 字符串数组 |
| `redacted_body` | 可选；PII 扫描 |
| `references_redacted` | 可选数组 |

### 4.7 `work_metrics`

| 字段 | 规则 |
|------|------|
| `turn_band` | 如 `1-2` / `3-5` / `6-10` / `11+` |
| `duration_band` | 如 `0-5m` / `5-15m` / `15-30m` / `30m+` |
| `tool_failure_band` | `0` / `1-2` / `3+` |
| `skill_patch_count_band` | `0` / `1` / `2+` |

---

## 5. 入库映射

### 5.1 `insights_raw_contributions`

| 列 | 来源 |
|----|------|
| `contribution_type` | `domain_work_package` |
| `pattern_id` | `skill.pattern_id` |
| `domain_key` | `domain_poi.domain_key` |
| `resolution_verdict` | `resolution.verdict` |
| `resolution_confidence_band` | `resolution.confidence_band` |
| `resolution_evidence_tier` | `resolution.evidence_tier` |
| `reportable_signals` | `resolution.signal_codes` JSON |
| `topic_keys` | NULL（v3 不用） |

### 5.2 `insights_resolution_facts`

每条 accepted contribution 插入一行（与 raw 同事务）。

---

## 6. 聚合 Job — `insights_domain_skill_edges`

触发：ingest 后异步或定时（5min）。

```
weight = CASE verdict
  WHEN solved_confirmed THEN 1.0
  WHEN solved_inferred  THEN 0.85
  WHEN partial          THEN 0.4
  WHEN failed           THEN -0.5
  ELSE 0.2
END * CASE confidence_band WHEN high THEN 1.0 WHEN medium THEN 0.7 ELSE 0.4 END

UPSERT edge (domain_key, pattern_cluster_id)
  co_occurrence_count += 1
  weighted_success_score += weight
  distinct_installations = COUNT DISTINCT installation_id  -- 子查询维护
```

**Standard Skill 候选**（P2）：`verdict IN (solved_confirmed, solved_inferred) AND evidence_tier >= B AND distinct_installations >= K`。

---

## 7. Payload 完整示例

```json
{
  "schema_version": 1,
  "work_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "session_id_hash": "a665a45920422f9d417e4867efdc4fb8a04a1f3fff1fa07e998e86f7f7a27ae3",
  "domain_poi": {
    "domain_key": "finance.reconciliation",
    "taxonomy_code": "finance.accounting.reconciliation",
    "problem_class": "operational",
    "problem_statement_redacted": "Multi-ledger reconciliation variance analysis",
    "difficulty_band": "high"
  },
  "resolution": {
    "verdict": "solved_confirmed",
    "confidence_band": "high",
    "evidence_tier": "A",
    "user_feedback_band": "explicit_positive",
    "objective_check_band": "pass",
    "signal_codes": [
      "user_explicit_positive",
      "closure_without_followup",
      "objective_test_pass",
      "skill_patched_this_session"
    ],
    "recovery_attempted": false
  },
  "skill": {
    "pattern_id": "abc123...64hex",
    "display_name": "ERP Reconciliation Workflow",
    "name_slug": "erp-reconciliation-workflow",
    "binding_role": "primary",
    "domain_keys": ["finance.reconciliation"],
    "description_redacted": "Steps to locate ledger mismatches",
    "structure": {
      "headings": ["Overview", "Steps"],
      "step_count": 4,
      "mentions_subagent": false,
      "mentions_cron": false,
      "mentions_mcp": false
    },
    "tool_chain": ["execute_code", "skill_manage"],
    "trigger_hints": { "slash_command": "erp-reconciliation-workflow" },
    "provenance": "agent_created",
    "content_version": "def456...64hex",
    "redacted_body": "## Steps\n1. ...",
    "references_redacted": []
  },
  "work_metrics": {
    "turn_band": "5-10",
    "duration_band": "15-30m",
    "tool_failure_band": "0",
    "skill_patch_count_band": "1"
  }
}
```

---

## 8. PII 扫描（继承 v2 + 扩展）

对 `problem_statement_redacted`、`display_name`、`description_redacted`、`redacted_body` 及 references 全文：

| 规则 | reason |
|------|--------|
| email 模式 | `pii_detected` |
| `sk-` 密钥 | `pii_detected` |
| `~/`、`/home/`、`C:\Users\` | `pii_detected` |
| `git@...git` | `pii_detected` |

---

## 9. OSS 路径

```
insights/raw/{yyyy}/{mm}/{dd}/{content_hash}.json.gz
insights/bodies/{pattern_id}/{content_version}.txt
```

大 payload：`redacted_body` 剥离后上 OSS body key（同 v2）。

---

## 10. Admin API 调整（`/admin/api/v1/insights/*`）

### 10.1 贡献列表 `GET .../contributions`

| 查询参数 | 说明 |
|----------|------|
| `type` | 固定 `domain_work_package` |
| `domain_key` | 前缀过滤 |
| `verdict` | 多选 comma-separated |
| `evidence_tier_min` | `A`/`B`/`C` |
| `since` / `until` | 时间范围 |

**列表主列**：`domain_poi.problem_statement_redacted`、`resolution.verdict`、`skill.display_name`、`resolution.evidence_tier`。

### 10.2 贡献详情

返回完整 payload（OSS 代理 `redacted_body`）。

### 10.3 统计 `GET .../stats/resolution`

```json
{
  "by_verdict": { "solved_confirmed": 120, "partial": 45, "failed": 12 },
  "by_evidence_tier": { "A": 80, "B": 90, "C": 30 },
  "by_domain_key_top": [{ "domain_key": "finance.reconciliation", "count": 34 }]
}
```

### 10.4 领域 × Skill 边 `GET .../domain-skill-edges`

供 OPS UI 共现图；`distinct_installations < K` 时返回 `suppressed: true`。

---

## 11. OPS UI 调整要点

| 页面 | v3 变更 |
|------|---------|
| 总览 | 增加「确认解决率」「A/B tier 占比」卡片 |
| 接入监控 | 拒绝原因 + verdict 分布 |
| 兴趣分析 → **领域分析** | 按 `domain_key` 聚合，展示 verdict 漏斗 |
| 技能模式 | 默认 filter：`verdict IN (solved_confirmed,solved_inferred) AND evidence_tier >= B` |
| 行业 Skill 草稿 | 创建时展示 `signal_codes` 分布、weighted_success_score |
| 合规 | 不变 |

---

## 12. Go struct 参考

```go
type DomainWorkPackagePayload struct {
    SchemaVersion  int              `json:"schema_version"`
    WorkID         string           `json:"work_id"`
    SessionIDHash  string           `json:"session_id_hash"`
    DomainPoi      DomainPoi        `json:"domain_poi"`
    Resolution     Resolution       `json:"resolution"`
    Skill          WorkPackageSkill `json:"skill"`
    WorkMetrics    WorkMetrics      `json:"work_metrics"`
}

type Resolution struct {
    Verdict             string   `json:"verdict"`
    ConfidenceBand      string   `json:"confidence_band"`
    EvidenceTier        string   `json:"evidence_tier"`
    UserFeedbackBand    string   `json:"user_feedback_band"`
    ObjectiveCheckBand  *string  `json:"objective_check_band,omitempty"`
    SignalCodes         []string `json:"signal_codes"`
    RecoveryAttempted   bool     `json:"recovery_attempted"`
}
```

Rust 对照：`crates/hermes-insights/src/types.rs`。

---

## 13. 实现 Checklist

- [ ] 迁移 SQL 002 + resolution_facts + domain_skill_edges
- [ ] `INSIGHTS_CONSENT_VERSION=2026-06-15`
- [ ] ingest 仅接受 `domain_work_package`
- [ ] 校验 §4 + PII §8
- [ ] 双写 raw + resolution_facts
- [ ] Admin API 列表/详情/统计
- [ ] OPS UI 领域分析 + resolution 筛选
- [ ] 聚类 job 加权 §6
- [ ] 联调：`hermes contribute preview` → `flush` → SQL 验收

### 验收 SQL

```sql
SELECT domain_key, resolution_verdict, resolution_evidence_tier,
       JSON_EXTRACT(payload_json, '$.skill.display_name') AS skill_title
FROM insights_raw_contributions
WHERE contribution_type = 'domain_work_package'
ORDER BY received_at DESC
LIMIT 10;

SELECT domain_key, verdict, evidence_tier, COUNT(*) AS n
FROM insights_resolution_facts
WHERE deleted_at IS NULL
GROUP BY domain_key, verdict, evidence_tier
ORDER BY n DESC
LIMIT 20;
```

---

## 14. DELETE `/v1/installations/{id}`

级联 soft-delete `insights_raw_contributions` 与 `insights_resolution_facts`（`deleted_at = now()`），OSS 异步删除（同 v2）。
