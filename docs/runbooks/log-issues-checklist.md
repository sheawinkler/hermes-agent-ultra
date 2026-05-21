# Hermes Agent Ultra — 运行日志问题检查清单

按优先级排查常见 WARN/ERROR。配置路径默认为 `~/.hermes/`。

## P0 — 立即（恢复核心能力）

### 1. Skill 被安全策略跳过

```yaml
# ~/.hermes/config.yaml
skills:
  enabled: []      # 非空 = 白名单；留空 = 允许全部（除 disabled）
  disabled: []     # 黑名单优先
```

- 审计 `~/.hermes/skills/` 下违规 skill：移除 `sudo`/`su`、硬编码 API key、`../` 路径遍历、外发 curl。
- 放宽扫描（仅开发环境）：`HERMES_SKILL_GUARD_MODE=relaxed` 或 `off`。安装与运行时共用 `skills_guard` 120 条规则（`SkillGuard` 门面）；strict 下已安装 skill 的 trust 来自 `skills/.hub/lock.json`。
- 验证：`hermes skills audit`，重启 gateway，日志中不应再出现 `Skipped N skill(s) due to security policy`。

### 2. vision_analyze 401 / invalid_api_key

在 `~/.hermes/.env` 至少配置其一：

```env
OPENROUTER_API_KEY=sk-or-...
# 或
HERMES_OPENAI_API_KEY=sk-...
OPENAI_API_KEY=sk-...
```

或在 `config.yaml` 配置 `auxiliary.vision`。确认模型支持 vision（如 `gpt-4o`）。

### 3. Chrome CDP 连接失败

```powershell
# 启动独立用户数据目录 + 调试端口
& "C:\Program Files\Google\Chrome\Application\chrome.exe" `
  --remote-debugging-port=9222 `
  --user-data-dir="$env:LOCALAPPDATA\hermes-chrome-debug"
```

`~/.hermes/.env`（Rust 与 Python 双写）：

```env
CHROME_CDP_URL=http://localhost:9222
BROWSER_CDP_URL=http://localhost:9222
```

验证：浏览器访问 `http://localhost:9222/json`，或 `hermes` 内 `/browser status`。

可选自动启动：`HERMES_BROWSER_AUTO_START=1`

### 4. execute_code：python3 not found

- 安装 Python 3.8+ 并加入 PATH。
- 或指定：`HERMES_PYTHON=C:\Path\To\python.exe`

验证：`python3 -c "print(1)"` 或 `py -3 -c "print(1)"`。

---

## P1 — 工具与 Hook（代码已加固，仍建议确认）

| 现象 | 处理 |
|------|------|
| `read_file` UTF-8 错误读 PNG | 对图片使用 `vision_analyze`，勿 `read_file` |
| `send_message` 缺 platform/recipient | Gateway 会话内会自动回退当前 channel |
| `Hook payload does not match` | 已对齐 `api_call_count`/`attempt`；更新自定义 hook 脚本时两种字段均可 |
| `clarify timed out after 300s` | 默认已改为 120s；可设 `HERMES_CLARIFY_TIMEOUT_SECS` |

---

## P2 — 可选

- **Honcho not configured**：需要长期记忆时配置 Honcho；否则可忽略 DEBUG。
- **WeCom websocket cmd= heartbeat**：正常，无需处理。
- **Idle session expired**：正常会话回收。
- **Capping delegate_task**：调高 delegate 并发配置（若需）。

---

## 快速诊断命令（Windows）

```powershell
where python3; where python; where py
curl -s http://localhost:9222/json/version
hermes skills audit
```
