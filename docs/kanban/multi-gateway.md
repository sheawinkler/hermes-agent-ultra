# Multi-Gateway Kanban Dispatch

Hermes Ultra can run multiple gateway processes on one machine, usually one per profile. Only one process should own Kanban dispatch/notifier responsibilities.

## Dispatch Owner

Leave the owning gateway at the default:

```yaml
kanban:
  dispatch_in_gateway: true
```

For every non-owning gateway, set:

```yaml
kanban:
  dispatch_in_gateway: false
```

Or set the environment override:

```sh
HERMES_KANBAN_DISPATCH_IN_GATEWAY=false
```

## Runtime Behavior

The Rust config loader parses `kanban.dispatch_in_gateway`, applies the env override, propagates the flag into the runtime gateway config, and includes it in gateway-agent cache signatures. Non-dispatch gateway profiles can still run platform adapters; they should not own Kanban dispatch duties.

The current Rust Kanban store is JSON-backed under Hermes home rather than the upstream Python SQLite board database, so there is no separate Rust per-board DB notifier loop to gate.
