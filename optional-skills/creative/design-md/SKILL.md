---
name: design-md
description: Author, lint, diff, and export DESIGN.md files (Google's open-source format for giving coding agents persistent design-system context). Use when building a design system, porting brand rules, enforcing consistency, or validating accessibility contrast.
version: 1.0.0
author: Hermes Agent Ultra
license: MIT
metadata:
  hermes:
    tags: [design, design-system, tokens, ui, accessibility, wcag, dtcg, tailwind]
    category: creative
---

# DESIGN.md

`DESIGN.md` is an open format from Google (`google-labs-code/design.md`) that
combines machine-readable design tokens with human-readable guidance in one
file.

Use this skill when users need:

- A structured design system spec an agent can follow repeatedly
- Design token normalization across projects
- CI-friendly validation of color contrast and token references
- Export paths into Tailwind or DTCG formats

## Expected file anatomy

1. YAML front matter
2. Markdown narrative sections

Typical token groups:

- `colors`
- `typography`
- `rounded`
- `spacing`
- `components`

Component variants should be sibling keys (for example:
`button-primary-hover`), not nested keys.

## Canonical section order

When sections exist, keep this order:

1. Overview
2. Colors
3. Typography
4. Layout
5. Elevation & Depth
6. Shapes
7. Components
8. Do's and Don'ts

## CLI workflow

Use the official CLI package:

```bash
npx -y @google/design.md lint DESIGN.md
npx -y @google/design.md diff DESIGN.md DESIGN-next.md
npx -y @google/design.md export --format tailwind DESIGN.md > tailwind.theme.json
npx -y @google/design.md export --format dtcg DESIGN.md > tokens.json
```

## Authoring rules

- Quote hex colors (example: `"#1A1C1E"`).
- Quote negative dimensions (example: `"-0.02em"`).
- Prefer token references in components (example: `{colors.primary}`).
- Keep component properties to supported names (backgroundColor, textColor,
  typography, rounded, padding, size, height, width).

## Recommended execution pattern

1. Generate or update `DESIGN.md`.
2. Run `lint`.
3. Fix broken references and accessibility warnings.
4. Export target format if needed.
5. Report what changed and why.

## Starter template

If user asks for a fresh spec, begin from:

`templates/starter.md`

## References

- Spec repo: https://github.com/google-labs-code/design.md
- NPM package: `@google/design.md`
