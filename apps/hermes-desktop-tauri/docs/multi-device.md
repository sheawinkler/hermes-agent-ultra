# Multi-device

One **PRIMARY** desktop per account runs the agent. Secondary devices:

1. Register via `POST /api/devices`
2. Discover primary via `_terra-hermes._tcp.local` (mDNS)
3. If offline: read-only cache, draft tasks queued locally

Cross-network: `wss://relay.terra.app/{device_id}` (Terra Cloud Relay).
