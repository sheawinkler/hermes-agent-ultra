# Parity fixtures

## Layout

- **`registry.json`** — lists **active** golden modules only; larger roadmap items stay in `PARITY_PLAN.md` until fixtures exist.
- **`<module>/*.json`** — one file per focus area; each file has `schema_version`, `cases[]`.
- **`pending/`** — excluded from `run_all_active_fixtures` (scaffolding / future work).

## Case schema

Each case may include:

- `id`, `op`, `input`, `expected`
- **`skip`: `true`** — ignored by the harness (placeholder until implementations exist).

## Running

```bash
cargo test -p hermes-parity-tests
```

Recursive run over all non-`pending` JSON: `hermes_parity_tests::run_all_active_fixtures`.

## Recording (Python)

```bash
python3 scripts/record_fixtures.py
```

Works without a Python checkout for **`checkpoint_shadow_dir_id`**; with sibling `research/hermes-agent`, also emits Anthropic adapter goldens.
