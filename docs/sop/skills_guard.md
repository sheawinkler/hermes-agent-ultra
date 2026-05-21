# SOP: `skills_guard`

| 字段 | 值 |
|------|-----|
| registry `id` | `skills_guard` |
| Python | `tools/skills_guard.py` |
| Rust | `crates/hermes-skills/src/skills_guard.rs` |
| Crate | `hermes-skills` |
| Fixtures | `crates/hermes-parity-tests/fixtures/skills_guard/*.json` |

## 验证

```bash
cargo build -p hermes-skills
cargo test -p hermes-parity-tests skills_guard
cargo clippy -p hermes-skills -- -D warnings
```

## 提交

```
parity(skills_guard): port from python@<commit>
```
