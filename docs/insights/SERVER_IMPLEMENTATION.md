# Insights Contribution — 服务端实现规格（从零）

> **⚠️ v3 迁移**：新客户端仅上传 `domain_work_package`。请优先实现 [SERVER_V3_DOMAIN_WORK_PACKAGE.md](./SERVER_V3_DOMAIN_WORK_PACKAGE.md)；下文 v2 内容供历史对照。

> **客户端（Hermes Rust）**：已实现，见 [CLIENT_IMPLEMENTATION.md](./CLIENT_IMPLEMENTATION.md)。  
> **服务端现状**：Go + MySQL 8 + 阿里云 OSS + Insights REST **已上线**；鉴权为 **必填** `Authorization: Bearer`。  
> **本文档**：表结构、业务逻辑、接口契约（供联调与运营侧对照）。  
> **Consent 版本**：`2026-05-29`（与客户端 `INSIGHTS_CONSENT_VERSION` 一致）

---

## 0. 实现范围

| 已有 | 说明 |
|------|------|
| Go Web、MySQL、OSS | 基础设施 |
| `POST /v1/insights/batch`、`DELETE /v1/installations/{id}` | 客户端 ingest（**已实现**） |
| Bearer 鉴权 | 见 §2.1，**所有** Insights 客户端接口必填 |

运营内网 Admin API / 聚合表见 [OPS_UI.md](./OPS_UI.md)（与客户端 Bearer 分离）。

**表名前缀**：统一 `insights_`，避免与业务库其它表冲突。

| 逻辑名 | 物理表名 |
|--------|----------|
| 安装实例 | `insights_installations` |
| 批次幂等 | `insights_ingest_batches` |
| 贡献明细 | `insights_raw_contributions` |

---

## 1. MySQL 8 — 建表 DDL（复制到迁移文件）

字符集 `utf8mb4`，引擎 `InnoDB`，时间 `DATETIME(3)` 存 UTC。

```sql
-- migrations/001_insights_core.up.sql

CREATE TABLE insights_installations (
    installation_id   CHAR(36)     NOT NULL COMMENT 'X-Installation-Id UUID',
    first_seen_at     DATETIME(3)  NOT NULL,
    last_seen_at      DATETIME(3)  NOT NULL,
    revoked_at        DATETIME(3)  NULL,
    client_version    VARCHAR(64)  NULL,
    PRIMARY KEY (installation_id),
    KEY idx_insights_installations_revoked (revoked_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE insights_ingest_batches (
    batch_id          CHAR(36)     NOT NULL,
    installation_id   CHAR(36)     NOT NULL,
    consent_version   VARCHAR(32)  NOT NULL,
    received_at       DATETIME(3)  NOT NULL,
    accepted_count    INT UNSIGNED NOT NULL DEFAULT 0,
    duplicate_count   INT UNSIGNED NOT NULL DEFAULT 0,
    rejected_count    INT UNSIGNED NOT NULL DEFAULT 0,
    PRIMARY KEY (batch_id),
    KEY idx_insights_batches_installation (installation_id, received_at),
    CONSTRAINT fk_insights_batches_installation
        FOREIGN KEY (installation_id) REFERENCES insights_installations (installation_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE insights_raw_contributions (
    id                  BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    content_hash        CHAR(64)        NOT NULL COMMENT 'sha256 hex, global dedup',
    batch_id            CHAR(36)        NOT NULL,
    installation_id     CHAR(36)        NOT NULL,
    contribution_type   ENUM('interest_snapshot','skill_pattern') NOT NULL,
    collected_at        DATETIME(3)     NOT NULL,
    received_at         DATETIME(3)     NOT NULL,
    payload_json        JSON            NULL COMMENT 'inline when small',
    oss_object_key      VARCHAR(512)    NULL,
    oss_body_key        VARCHAR(512)    NULL,
    pattern_id          VARCHAR(64)     NULL,
    topic_keys          JSON            NULL COMMENT '["lang:rust",...]',
    reject_reason       VARCHAR(32)     NULL COMMENT 'NULL = accepted row',
    deleted_at          DATETIME(3)     NULL COMMENT 'revoke soft-delete',
    PRIMARY KEY (id),
    UNIQUE KEY uk_insights_content_hash (content_hash),
    KEY idx_insights_raw_installation (installation_id, contribution_type),
    KEY idx_insights_raw_batch (batch_id),
    KEY idx_insights_raw_pattern (pattern_id),
    KEY idx_insights_raw_received (received_at),
    CONSTRAINT fk_insights_raw_batch
        FOREIGN KEY (batch_id) REFERENCES insights_ingest_batches (batch_id),
    CONSTRAINT fk_insights_raw_installation
        FOREIGN KEY (installation_id) REFERENCES insights_installations (installation_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
```

**说明**

- `insights_raw_contributions.reject_reason`：仅当该条在 batch 响应里为 `rejected` 时可不落库；**推荐** rejected 不落库，只出现在 HTTP 响应（表内行均为已接受）。
- `content_hash` 全局 UNIQUE：跨安装去重，重复计 `duplicates`。
- `batch_id` UNIQUE（PK）：重复 POST 同一 batch → **409**。

---

## 2. 鉴权（必填）

### 2.1 请求头

```
Authorization: Bearer <用户 JWT 或 flowy- API Key>
```

| 凭证类型 | 格式 | 说明 |
|----------|------|------|
| 用户 JWT | `eyJ...` | 运营平台登录态签发；客户端可**先写死在** `config.yaml`（见客户端文档） |
| Flowy API Key | `flowy-...` | 服务账号 / 自动化联调 |

| HTTP | 条件 |
|------|------|
| `401` | 缺少 `Authorization`、非 `Bearer` 前缀、token 无效或过期 |
| `403` | token 有效但无 Insights 写入权限（若服务端做 RBAC） |

**与 `X-Installation-Id` 的关系**：Bearer 标识**用户/租户**；`X-Installation-Id` 标识**Hermes 客户端安装实例**（匿名 UUID）。二者同时必填。

### 2.2 服务端配置项（Go 环境变量）

| 变量 | 示例 | 说明 |
|------|------|------|
| `MYSQL_DSN` | 已有 | 连接池 |
| `OSS_BUCKET` | 已有 | 对象存储 |
| `OSS_ENDPOINT` | 已有 | |
| `INSIGHTS_PAYLOAD_INLINE_MAX` | `4096` | 超过则 payload 上 OSS |
| `INSIGHTS_CONSENT_VERSION` | `2026-05-29` | 与客户端不一致 → 400 |
| JWT 校验 | 对接现有用户中心 | 校验签名、`exp`、issuer |

---

## 3. REST 路由（必须实现）

| 方法 | 路径 | 说明 |
|------|------|------|
| `POST` | `/v1/insights/batch` | 客户端 `endpoint` 配置为**完整 URL** |
| `DELETE` | `/v1/installations/{installation_id}` | 客户端从 batch URL 推导 base |

客户端 DELETE URL 推导逻辑（须兼容）：

```
endpoint = "https://api.example.com/v1/insights/batch"
base     = trim_suffix(endpoint, "/v1/insights/batch")
DELETE   = "{base}/v1/installations/{installation_id}"
```

---

## 4. `POST /v1/insights/batch` — 业务逻辑

### 4.1 请求 / 响应（与 Rust 一致）

**Headers**：`Content-Type: application/json`；**必填** `Authorization: Bearer`、`X-Installation-Id`；可选 `X-Client-Version`。

**Body**（`contributions[].type` 为 JSON 字段名 `type`）：

```json
{
  "batch_id": "550e8400-e29b-41d4-a716-446655440000",
  "consent_version": "2026-05-29",
  "contributions": [
    {
      "type": "interest_snapshot",
      "collected_at": "2026-05-29T12:00:00Z",
      "content_hash": "64位hex",
      "payload": { }
    }
  ]
}
```

**200 响应**：

```json
{
  "accepted": 1,
  "duplicates": 0,
  "rejected": [{ "content_hash": "...", "reason": "pii_detected" }]
}
```

| HTTP | 条件 |
|------|------|
| 400 | JSON 非法、缺 header、`consent_version` 不匹配 |
| 401 | Bearer 缺失或无效 |
| 409 | `batch_id` 已存在于 `insights_ingest_batches`（**客户端当成功**） |
| 422 | `contributions` 非空且 **全部** rejected |
| 200 | 至少 1 条 accepted 或 duplicate |

`reason` 枚举：`pii_detected` | `schema_violation` | `guard_high_severity` | `duplicate`（duplicate 也可只计数字不入 rejected 列表）。

### 4.2 处理流程（伪代码）

```
HandleBatch(req, headers):
  installationID = headers["X-Installation-Id"]
  if installationID == "" → 400

  if Authorization missing or invalid JWT / flowy- key → 401

  parse JSON → BatchRequest
  if consent_version != INSIGHTS_CONSENT_VERSION → 400

  BEGIN TRANSACTION

  upsertInstallation(installationID, headers["X-Client-Version"])
  if installation.revoked_at != NULL → 403  // 可选：已撤销不再接收

  if exists insights_ingest_batches where batch_id = req.batch_id:
    ROLLBACK; return 409

  insert insights_ingest_batches (batch_id, installation_id, consent_version, received_at=now)

  accepted, duplicates, rejected = 0, 0, []

  for each item in req.contributions:
    if item.type not in ("interest_snapshot", "skill_pattern"):
      rejected.append({hash: item.content_hash, reason: "schema_violation"})
      continue

    if exists insights_raw_contributions where content_hash = item.content_hash:
      duplicates++
      continue

    err = validatePayload(item.type, item.payload)
    if err != nil:
      rejected.append({hash: item.content_hash, reason: err})
      continue

    payloadForStore, ossKey, bodyKey = persistPayload(item)  // §6

    insert insights_raw_contributions (
      content_hash, batch_id, installation_id, contribution_type,
      collected_at, received_at, payload_json, oss_object_key, oss_body_key,
      pattern_id, topic_keys
    )
    accepted++

  update insights_ingest_batches set accepted_count, duplicate_count, rejected_count

  COMMIT

  if len(req.contributions) > 0 and accepted==0 and duplicates==0:
    return 422, body with rejected
  return 200, {accepted, duplicates, rejected}
```

### 4.3 Payload 校验规则

#### `interest_snapshot` → 结构体字段

| 字段 | 规则 |
|------|------|
| `topics` | 非空数组 |
| `topics[].topic_key` | 必填；**禁止** `path:`、`keyword:` 前缀 |
| `topics[].namespace` | 必填 |
| `topics[].weight_band` | `low` \| `med` \| `high` |
| `topics[].evidence_band` | `1-2` \| `3-5` \| `6+` |
| `co_topics` | 可选字符串数组 |
| **禁止顶层字段** | `summary`、`label` |

写入冗余：`topic_keys` = 所有 `topics[].topic_key` 的 JSON 数组。

#### `skill_pattern` → 结构体字段

| 字段 | 规则 |
|------|------|
| `pattern_id` | 64 hex |
| `name_slug` | 非空，仅 `[a-z0-9-]` |
| `structure.step_count` | ≥ 0 |
| `provenance` | `agent_created` \| `user_created` |
| `content_version` | 非空 |
| `redacted_body` | 可选；非空时须通过 PII 扫描（§5） |

写入冗余：`pattern_id` = payload.pattern_id。

### 4.4 PII 二次扫描（服务端必须）

对 **整个 payload JSON 字符串** + `redacted_body`（若有）执行：

| 规则 | reason |
|------|--------|
| 含 `@` 且 `.` 且非 `[REDACTED` 子串 | `pii_detected` |
| 含 `sk-` 且非 `[REDACTED` | `pii_detected` |
| 匹配 `(?i)(~\/\|/home/[\w.-]+\|C:\\Users\\[\w.-]+\\)` | `pii_detected` |
| 匹配 `git@[\w.-]+:[\w./-]+\.git` | `pii_detected` |
| interest `topic_key` 以 `path:` / `keyword:` 开头 | `schema_violation` |

Golden：Rust 仓库 `crates/hermes-insights/tests/fixtures/skill_with_pii.md` 经客户端脱敏后仍可能漏网，服务端须 reject。

---

## 5. `DELETE /v1/installations/{installation_id}` — 业务逻辑

```
HandleRevoke(installationID, headers):
  if Bearer 校验失败 → 401
  if path param != headers["X-Installation-Id"] → 403  // 建议：只能删自己

  BEGIN TRANSACTION

  row = select from insights_installations where installation_id = ?
  if row == nil → 404  // 客户端将 404 视为成功

  update insights_installations set revoked_at = now(3) where installation_id = ?

  keys = select oss_object_key, oss_body_key from insights_raw_contributions
         where installation_id = ? and deleted_at is null
         and (oss_object_key is not null or oss_body_key is not null)

  update insights_raw_contributions
    set deleted_at = now(3),
        installation_id = '00000000-0000-0000-0000-000000000000'  -- 匿名化
    where installation_id = ?

  COMMIT

  go asyncDeleteOSS(keys)  // 批量 DeleteObject，失败写日志重试

  return 204 No Content
```

**说明**：`payload_json` 小对象可保留用于统计（已匿名 `installation_id`）；合规要求「彻底删除」时，同步 `DELETE` 行并删 OSS。

---

## 6. OSS 读写（业务逻辑）

### 6.1 路径约定

```
insights/raw/{yyyy}/{mm}/{dd}/{content_hash}.json.gz
insights/bodies/{pattern_id}/{content_version}.txt
```

### 6.2 `persistPayload(item)` 逻辑

```
canonical = json.Marshal(item.payload without redacted_body field)
size = len(canonical)

bodyKey = null
if item.type == skill_pattern and payload.redacted_body != "":
  bodyKey = "insights/bodies/{pattern_id}/{content_version}.txt"
  OSS.PutObject(bodyKey, payload.redacted_body)
  payload.redacted_body = null  // 不入 MySQL JSON

if size <= INSIGHTS_PAYLOAD_INLINE_MAX:
  return payload, ossKey=null, bodyKey

ossKey = "insights/raw/{date}/{content_hash}.json.gz"
OSS.PutObject(ossKey, gzip(canonical))
return null, ossKey, bodyKey   // 大对象 MySQL payload_json = NULL
```

---

## 7. Go 代码结构建议

```
internal/insights/
  handler.go      // gin/chi 路由注册
  batch.go        // HandleBatch
  revoke.go       // HandleRevoke
  validate.go     // schema + PII
  store_mysql.go  // SQL
  store_oss.go    // Put/Delete
  types.go        // 与 Rust types.rs 对齐的 struct
```

**types.go 关键 JSON 标签**（`type` 是 Go 关键字，用 `json:"type"`）：

```go
type ContributionEnvelope struct {
    Type         string          `json:"type"`
    CollectedAt  string          `json:"collected_at"`
    ContentHash  string          `json:"content_hash"`
    Payload      json.RawMessage `json:"payload"`
}

type BatchRequest struct {
    BatchID         string                 `json:"batch_id"`
    ConsentVersion  string                 `json:"consent_version"`
    Contributions   []ContributionEnvelope `json:"contributions"`
}

type BatchResponse struct {
    Accepted   uint32               `json:"accepted"`
    Duplicates uint32               `json:"duplicates"`
    Rejected   []RejectedContribution `json:"rejected"`
}
```

---

## 8. 实现 Checklist（按顺序）

### Phase A — 数据库

- [ ] 执行 `001_insights_core.up.sql`（goose / golang-migrate）
- [ ] 验证外键、UNIQUE(`content_hash`)、PK(`batch_id`)

### Phase B — `POST /v1/insights/batch`

- [ ] 注册路由 + 中间件（Installation-Id、Bearer）
- [ ] 实现 §4.2 事务流程
- [ ] 实现 §4.3 schema 校验
- [ ] 实现 §4.4 PII 扫描
- [ ] 实现 §6 OSS 分支
- [ ] 单测：409、duplicate、全 reject → 422、混合 200

### Phase C — `DELETE /v1/installations/{id}`

- [ ] 实现 §5 撤销 + 异步 OSS 删除
- [ ] 单测：404 幂等

### Phase D — 与 Hermes 客户端联调

- [ ] `hermes config set insights.contribution.endpoint https://<host>/v1/insights/batch`
- [ ] `hermes config set insights.contribution.auth_token <JWT 或 flowy- API Key>`（或 `export HERMES_INSIGHTS_TOKEN=...`）
- [ ] `hermes contribute enable` → `preview` → `flush`（`contribute status` 应显示 `Upload ready: true`）
- [ ] `hermes contribute revoke` → 查 MySQL `revoked_at`、OSS 对象已删

---

## 9. 运营平台前端（展示规格）

页面结构、字段映射、内网 Admin API、权限与分期见 **[OPS_UI.md](./OPS_UI.md)**。

| Phase | 前端模块 | 后端依赖 |
|-------|----------|----------|
| P0 | 总览、接入监控、合规 | 核心三表 + `/admin/api/v1/insights/stats|batches|contributions` |
| P1 | 兴趣分析、技能模式 | 聚合表 + k-匿名 |
| P2 | 行业 Skill 草稿、词表 | 聚类 job + drafts 表 |

---

## 10. 后续迭代（非 P0 客户端接口）

| 项 | 说明 |
|----|------|
| `GET /v1/insights/taxonomy/manifest` | 客户端暂未强制 |
| k-匿名 staging 表 + 定时任务 | 支撑 OPS_UI 兴趣/技能排行 |
| 聚类 / 运营审核 API | 支撑 OPS_UI 草稿流 |

---

## 11. 客户端类型对照

Rust 定义：`crates/hermes-insights/src/types.rs`  
HTTP 客户端：`crates/hermes-insights/src/client.rs`（409 当成功、DELETE base URL 推导）
