# Insights 服务端同步 — 可读 Payload（v2）

> **⚠️ 已 superseded**：当前客户端见 [SERVER_V3_DOMAIN_WORK_PACKAGE.md](./SERVER_V3_DOMAIN_WORK_PACKAGE.md)（`domain_work_package` + resolution 标记）。

> **触发原因**：客户端改为上传 **脱敏后的可读明文**（POI label/summary、Skill 标题与正文），**不再**上传 `interest:<hex>` 等仅客户端可解析的 id。  
> **客户端契约版本**：`consent_version` 仍为 `2026-05-29`；payload **schema v2**（见下文）。  
> **关联**：[SERVER_IMPLEMENTATION.md](./SERVER_IMPLEMENTATION.md) ingest 流程不变，**校验字段与 DB 冗余列需调整**。

---

## 1. 变更摘要

| 项 | v1（旧） | v2（新，当前客户端） |
|----|----------|----------------------|
| POI `topic_key` | 可能为 `interest:0062d40f…` | **`lang:rust` / `tech:*` / `topic:<slug-from-label>`** |
| POI 可读字段 | 无 | **`label_redacted`**（必填）、**`summary_redacted`**（可选） |
| POI `co_topics` | 本地 topic id | **脱敏 label 字符串列表** |
| Skill 展示 | 主要 `name_slug` + `pattern_id` | **`display_name`**（必填）+ `description_redacted` |
| Skill 正文 | 默认不上传 | **默认上传 `redacted_body`**（去掉 References 段、PII 占位） |
| Skill 关联兴趣 | `linked_interest_keys` | **`linked_interest_labels`**（脱敏 label） |
| 服务端解析 | 无法还原 `interest:<hex>` | **直接展示 `label_redacted` / `display_name` / `redacted_body`** |

`pattern_id`、`content_hash` 仍保留，仅用于 **去重**，运营 UI **不应**作为主展示字段。

---

## 2. `interest_snapshot` Payload（v2）

```json
{
  "topics": [
    {
      "topic_key": "topic:beijing-dialect",
      "label_redacted": "Beijing dialect",
      "summary_redacted": "User prefers casual Beijing phrasing",
      "namespace": "topic",
      "weight_band": "high",
      "evidence_band": "6+",
      "tags": ["declared"],
      "taxonomy_hints": []
    },
    {
      "topic_key": "lang:rust",
      "label_redacted": "Rust",
      "summary_redacted": "Systems programming",
      "namespace": "lang",
      "weight_band": "low",
      "evidence_band": "1-2",
      "tags": ["lang", "rust"],
      "taxonomy_hints": ["software.backend.rust"]
    }
  ],
  "co_topics": ["Beijing dialect", "Rust"],
  "collected_at": "RFC3339"
}
```

### 2.1 服务端校验调整

| 规则 | 动作 |
|------|------|
| 每条 topic **必须有** `label_redacted`（非空） | 缺则 `schema_violation` |
| **禁止** `topic_key` 匹配 `interest:[0-9a-f]{12,}` | `schema_violation` |
| **禁止** `co_topics` 含 `interest:` 前缀 | `schema_violation` |
| `summary_redacted` | 可选；PII 二次扫描 |
| 仍禁止 | 原始 `summary` 字段、邮箱、路径、`sk-` |

### 2.2 MySQL 冗余列建议

`insights_raw_contributions` 解析后写入：

| 列 | 来源 |
|----|------|
| `topic_keys` | JSON 数组：`topics[].topic_key`（可读 key） |
| 新增 `topic_labels` JSON | `topics[].label_redacted`（**运营列表主展示**） |
| `pattern_id` | skill 不变 |

运营排行 / UI：**按 `topic_labels` 或 `label_redacted` 聚合**，不要按 `interest:<hex>`。

---

## 3. `skill_pattern` Payload（v2）

```json
{
  "pattern_id": "sha256hex",
  "display_name": "Conversational Style Adaptation",
  "name_slug": "conversational-style",
  "category": "communication",
  "description_redacted": "Adapt communication tone...",
  "structure": { "headings": ["..."], "step_count": 4, "mentions_mcp": false },
  "tool_chain": ["skill_manage"],
  "trigger_hints": { "slash_command": "conversational-style" },
  "provenance": "agent_created",
  "content_version": "sha256hex",
  "linked_interest_labels": ["Rust", "Beijing dialect"],
  "redacted_body": "## Steps\n1. ...",
  "references_redacted": [
    {
      "relative_path": "references/api-guide.md",
      "content_redacted": "## API\nPublic endpoint usage..."
    }
  ]
}
```

### 3.1 服务端校验调整

| 规则 | 动作 |
|------|------|
| **必须有** `display_name` | 缺则 `schema_violation` |
| **`references_redacted[]`** | 可选；每项含 `relative_path`（`references/`、`templates/`、`scripts/`、`assets/` 下相对路径）、`content_redacted`；仅文本类扩展名，二进制 assets 不上传 |
| **废弃字段** `linked_interest_keys` | 可忽略；v2 用 `linked_interest_labels` |
| PII 扫描 | `display_name`、`description_redacted`、`redacted_body` 全文 |

### 3.2 OSS / 存储

- `redacted_body` 仍建议 **单独 OSS 对象**（超阈值时），MySQL 存 `oss_body_key`。
- 运营详情页 **主展示**：`display_name` → `description_redacted` → Tab「正文」读 `redacted_body`。

---

## 4. 运营 UI 展示字段（更新 OPS_UI）

| 模块 | 主列（v2） |
|------|------------|
| 兴趣 Topic 列表 | **`label_redacted`**、`summary_redacted`、`weight_band` |
| Topic 详情 | 同上 + `topic_key`（次要） |
| Skill 列表 | **`display_name`**、`category`、`description_redacted` 摘要 |
| Skill 详情 | `display_name`、`redacted_body`、`linked_interest_labels` |

---

## 5. 响应体（请一并修复）

客户端需要真实统计，**不要**只返回 `{ "code": 200 }`：

```json
{
  "accepted": 2,
  "duplicates": 0,
  "rejected": []
}
```

或：

```json
{
  "code": 0,
  "data": {
    "accepted": 2,
    "duplicates": 0,
    "rejected": []
  }
}
```

**入库失败必须非 2xx**，避免 HTTP 200 但表空。

---

## 6. 服务端实现 Checklist

- [ ] 更新 Go struct：`InterestTopicFingerprint` 增加 `LabelRedacted`、`SummaryRedacted`；Skill 增加 `DisplayName`、`LinkedInterestLabels`
- [ ] 校验：拒绝 `interest:<hex>` 形态 `topic_key`
- [ ] 入库：冗余 `topic_labels` JSON（或从 payload 解析）
- [ ] 运营 Admin API：列表/详情返回可读字段
- [ ] 迁移：旧 v1 行可保留；新行按 v2 展示
- [ ] 联调：客户端 `hermes contribute preview` → 确认无 `interest:…` hash → `flush` → 查表

---

## 7. 联调验收 SQL

```sql
SELECT contribution_type,
       JSON_EXTRACT(payload_json, '$.topics[0].label_redacted') AS first_label,
       JSON_EXTRACT(payload_json, '$.display_name') AS skill_title
FROM insights_raw_contributions
ORDER BY received_at DESC
LIMIT 10;
```

期望：`first_label` / `skill_title` 为 **人类可读中文或英文**，而非 hex id。
