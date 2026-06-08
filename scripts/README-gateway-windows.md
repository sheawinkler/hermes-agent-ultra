# Windows 网关启动（Hermes Agent Ultra）

## 长期用法（推荐）

在仓库根目录 PowerShell 中：

```powershell
.\scripts\start-gateway-ultra.ps1
```

脚本会：

1. 将 `HERMES_HOME` 设为 `%LOCALAPPDATA%\hermes-agent-ultra`（若仅有旧的 `\hermes` 目录则自动复制迁移）
2. 将日志写入 `%LOCALAPPDATA%\hermes-agent-ultra\logs\hermes.log`
3. 启动前结束冲突的 Python `hermes gateway` 进程
4. 运行 `hermes-agent-ultra.exe -C <HERMES_HOME> gateway run`

## 其它命令

```powershell
# 停止网关
.\scripts\start-gateway-ultra.ps1 -Stop

# 更详细的 Discord 调试日志
.\scripts\start-gateway-ultra.ps1 -VerboseLog

# 指定二进制路径
.\scripts\start-gateway-ultra.ps1 -Binary C:\path\to\hermes-agent-ultra.exe
```

## 复测私聊时看日志

另开一个终端：

```powershell
Get-Content "$env:LOCALAPPDATA\hermes-agent-ultra\logs\hermes.log" -Wait -Tail 30 |
  Select-String "Discord|inbound|Failed to route"
```

每条私聊应出现 `Discord inbound message accepted`。

## 可选：永久设置 HERMES_HOME

若希望所有 `hermes` 命令默认读 `%LOCALAPPDATA%\hermes-agent-ultra`：

```powershell
[Environment]::SetEnvironmentVariable(
  'HERMES_HOME',
  "$env:LOCALAPPDATA\hermes-agent-ultra",
  'User'
)
```

设置后需重新打开终端。旧的 `%LOCALAPPDATA%\hermes` 目录会保留作备份。

## 未改动的配置

- `streaming.enabled` 仍为 `true`（未做临时关闭）
- 代理 / Discord 网络需在 Clash 等工具中自行配置直连规则
