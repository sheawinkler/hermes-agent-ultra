# Terra installer size (W3)

Baseline estimates for the Terra Tauri desktop bundle (no bundled Chromium; system browser driver is separate).

| Artifact | Approx. size | Notes |
|----------|-------------|-------|
| NSIS `.exe` (Windows) | 8–15 MB | WebView2 bootstrapper may add ~2 MB on first install |
| `.dmg` (macOS) | 10–18 MB | Hardened runtime + entitlements; no embedded backend |
| `.deb` / AppImage (Linux) | 12–20 MB | Depends on linked GTK/WebKit stack |

`hermes-http` is shipped adjacent to the app or installed as a per-user Windows service / macOS LaunchAgent. The service binary adds ~15–25 MB when copied beside the installer payload.

## OTA / partial updates (planned)

- **V1**: full installer replace via GitHub Releases or Terra Cloud CDN.
- **V2**: evaluate `bsdiff`/`Courgette`-style deltas for the Tauri shell only; backend (`hermes-http`) remains a separate artifact with its own semver.
- Delta updates should target the frontend `dist/` bundle and Rust shell separately to keep rollback simple.

## Size budget targets

- Desktop installer (compressed): **< 25 MB** without backend binary.
- Mobile APK/IPA (W4): **< 5 MB** JS bundle; native shell per platform store limits.

Measure after each release:

```bash
cd apps/hermes-desktop-tauri
npm run tauri:build
ls -lh src-tauri/target/release/bundle/nsis/*.exe
```
