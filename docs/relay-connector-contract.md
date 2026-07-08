# Relay Connector Contract

Status: upstream experimental connector contract, tracked for parity.

The upstream relay connector model defines an outbound WebSocket from a Hermes
gateway to a connector service. The connector owns platform-specific sockets,
normalizes inbound events, strips platform credentials at the edge, and sends
sanitized `MessageEvent` frames back over the authenticated WebSocket.

Hermes Ultra's current Rust runtime does not register this experimental relay
adapter as a first-class platform. Supported messaging behavior is owned by
Rust-native gateway adapters such as Telegram, Discord, Slack, Signal, Matrix,
WeCom, WhatsApp, ntfy, Home Assistant, email, SMS, and API server surfaces.

If the relay connector is ported, the Rust contract must preserve these
upstream invariants:

- The gateway dials out; hosted gateways do not need a public inbound port.
- The connector returns a capability descriptor before events flow.
- Relay identity-token provisioning supports generic OIDC
  `client_credentials` request contracts for self-hosted IdPs, while preserving
  Nous Portal as the default identity mode when no IdP token endpoint is
  configured.
- Inbound events use the same session-key discriminators as native platform
  adapters.
- Relay source metadata uses `scope_id` as the canonical platform-neutral
  tenant discriminator; `guild_id` is a deprecated alias that Rust readers
  dual-read and Rust writers dual-write during the migration overlap.
- Platform signatures, encrypted payloads, and follow-up tokens are verified or
  vaulted by the connector, never leaked into the gateway.
- Interrupts route by session key and cancel only the active turn for that
  session.
- Unknown additive descriptor fields are ignored for forward compatibility.

Until a Rust relay adapter is scoped, the transport itself remains a parity
reference rather than an active runtime API. The Rust gateway crate does own the
stable relay protocol and identity-token request contracts used to evaluate
future activation safely.
