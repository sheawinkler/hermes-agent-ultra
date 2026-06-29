# OEM Variants

Build a branded Terra client from `variants/<id>/variant.json`.

```bash
node scripts/apply-variant.mjs default
npm run tauri:build
```

Each variant may override branding, icons, and i18n merges. Output: `src-tauri/tauri.conf.<id>.json`.
