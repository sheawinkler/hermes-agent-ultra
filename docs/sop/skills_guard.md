# SOP: `skills_guard`

| 字段 | 值 |
|------|-----|
| registry `id` | `skills_guard` |
| Python | `tools/skills_guard.py` |
| Rust | `crates/hermes-skills/src/skills_guard.rs`（引擎）+ `guard.rs`（`SkillGuard` 门面） |
| Crate | `hermes-skills` |
| Fixtures | `crates/hermes-parity-tests/fixtures/skills_guard/*.json` |

## 运行时

- `SkillGuard::scan_security_with_policy` 在 strict 下使用 `should_allow_install`（与安装一致）。
- `source` 来自 `hub_lock::resolve_scan_source`（读 `~/.hermes/skills/.hub/lock.json`）；未登记 skill 按 community 处理。

## 验证

```bash
cargo build -p hermes-skills
cargo test -p hermes-skills hub_lock
cargo test -p hermes-parity-tests skills_guard
cargo check -p hermes-agent
```

## 提交

```
parity(skills_guard): port from python@<commit>
```
