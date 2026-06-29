# Terra Architecture

## Topology

```
Tauri Desktop (React) ──invoke──► src-tauri commands
       │                              │
       │ HTTP/WS                      ├── hermes_backend (spawn hermes-http)
       └──────────────────────────────► hermes-http :8787
                                              ├── /api/tasks + WS multiplex
                                              ├── Gateway + AgentLoop
                                              └── hermes-tasks (SQLite)
```

## Design choices

- **No dylib**: `hermes-http` runs as a separate process; Tauri spawns it on demand (lazy start).
- **Task-centric**: `hermes-tasks` owns events/turns/artifacts; UI subscribes via `/api/tasks/{id}/stream`.
- **Single-desktop-primary**: secondary devices use mDNS + optional Cloud Relay (W14).

## Build

```bash
cargo build -p hermes-http
cd apps/hermes-desktop-tauri && npm run tauri:dev
```
