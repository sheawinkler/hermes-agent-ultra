# Vertical authoring

Author verticals under `crates/hermes-verticals/bundled/<id>/`:

- `VERTICAL.md` — frontmatter: `id`, `name`, `category`, `task_category`, `default_tier`
- `persona.{lang}.md` or `[[persona.blocks]]` in VERTICAL.md
- `starters.json` — home screen prompts
- `datasources.json` — optional datasource bindings

Run `cargo test -p hermes-verticals` after changes.
