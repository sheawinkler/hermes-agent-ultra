---
sidebar_position: 7
title: "Docker"
description: "Running Hermes Agent in Docker and using Docker as a terminal backend"
---

# Hermes Agent — Docker

There are two distinct ways Docker intersects with Hermes Agent:

1. **Running Hermes IN Docker** — the agent itself runs inside a container (this page's primary focus)
2. **Docker as a terminal backend** — the agent runs on your host but executes every command inside a single, persistent Docker sandbox container that survives across tool calls, `/new`, and subagents for the life of the Hermes process (see [Configuration → Docker Backend](./configuration.md#docker-backend))

This page covers option 1. The Ultra container stores all user data (config, API keys, sessions, skills, memories) in a single directory mounted from the host at `/data`. The image itself is stateless and can be rebuilt or upgraded without losing any configuration.

## Quick start

Build the local Rust image, then create a data directory on the host and start the container interactively to run the setup wizard:

```sh
docker build -t hermes-agent-ultra .
mkdir -p ~/.hermes-agent-ultra
docker run -it --rm \
  -v ~/.hermes-agent-ultra:/data \
  hermes-agent-ultra setup
```

This drops you into the setup wizard, which will prompt you for your provider credentials and write them under `~/.hermes-agent-ultra`. You only need to do this once. It is highly recommended to set up a chat system for the gateway to work with at this point.

## Running in gateway mode

Once configured, run the container in the background as a persistent gateway (Telegram, Discord, Slack, WhatsApp, etc.):

```sh
docker run -d \
  --name hermes-agent-ultra \
  --restart unless-stopped \
  --network host \
  -v ~/.hermes-agent-ultra:/data \
  -e HERMES_UID="$(id -u)" \
  -e HERMES_GID="$(id -g)" \
  hermes-agent-ultra gateway run
```

Port 8642 exposes the gateway's [OpenAI-compatible API server](./features/api-server.md) and health endpoint. It's optional if you only use chat platforms (Telegram, Discord, etc.), but required if you want the dashboard or external tools to reach the gateway.

Note: the API server is gated on `API_SERVER_ENABLED=true`. To expose it beyond `127.0.0.1` inside the container, also set `API_SERVER_HOST=0.0.0.0` and an `API_SERVER_KEY` (minimum 8 characters — generate one with `openssl rand -hex 32`). Example:

```sh
docker run -d \
  --name hermes-agent-ultra \
  --restart unless-stopped \
  --network host \
  -v ~/.hermes-agent-ultra:/data \
  -e API_SERVER_ENABLED=true \
  -e API_SERVER_HOST=0.0.0.0 \
  -e API_SERVER_KEY=your_api_key_here \
  -e API_SERVER_CORS_ORIGINS='*' \
  hermes-agent-ultra gateway run
```

Opening any port on an internet facing machine is a security risk. You should not do it unless you understand the risks.

## Running the dashboard

Ultra runs the dashboard through the Rust `hermes dashboard` command. Keep the default loopback bind for local use:

```sh
docker run -it --rm \
  --network host \
  -v ~/.hermes-agent-ultra:/data \
  -e HERMES_UID="$(id -u)" \
  -e HERMES_GID="$(id -g)" \
  hermes-agent-ultra dashboard --host 127.0.0.1 --no-open
```

For persistent deployments, run the dashboard as the separate service shown in this repository's `docker-compose.yml`. The compose service shares the same `/data` volume and host network namespace as the gateway, and binds the dashboard to `127.0.0.1` by default.

Binding the dashboard/API surface to `0.0.0.0` requires an explicit `--insecure` acknowledgement on `hermes dashboard`, but that flag does **not** disable API authentication. The Rust API server still refuses network-accessible binds without `API_SERVER_KEY`.

:::note
There is no `HERMES_DASHBOARD_INSECURE` Docker escape hatch in Ultra's Rust/tini entrypoint. Use loopback, SSH/Tailscale, or a real `API_SERVER_KEY` for any network-accessible bind.
:::

## Running interactively (CLI chat)

To open an interactive chat session against a running data directory:

```sh
docker run -it --rm \
  -v ~/.hermes-agent-ultra:/data \
  hermes-agent-ultra
```

Or if you have already opened a terminal in your running container, just run:

```sh
hermes
```

## Persistent volumes

The `/data` volume is the single source of truth for all Hermes state. It maps to your host's `~/.hermes-agent-ultra/` directory in the examples above and contains:

| Path | Contents |
|------|----------|
| `.env` | API keys and secrets |
| `config.yaml` | All Hermes configuration |
| `SOUL.md` | Agent personality/identity |
| `sessions/` | Conversation history |
| `memories/` | Persistent memory store |
| `skills/` | Installed skills |
| `cron/` | Scheduled job definitions |
| `hooks/` | Event hooks |
| `logs/` | Runtime logs |
| `skins/` | Custom CLI skins |

:::warning
Never run two Hermes **gateway** containers against the same data directory simultaneously — session files and memory stores are not designed for concurrent write access.
:::

## Multi-profile support

Hermes supports [multiple profiles](../reference/profile-commands.md) — separate Hermes home directories that let you run independent agents (different SOUL, skills, memory, sessions, credentials) from a single installation. **When running under Docker, using Hermes' built-in multi-profile feature is not recommended.**

Instead, the recommended pattern is **one container per profile**, with each container bind-mounting its own host directory as `/data`:

```sh
# Work profile
docker run -d \
  --name hermes-ultra-work \
  --restart unless-stopped \
  --network host \
  -v ~/.hermes-ultra-work:/data \
  -e HERMES_UID="$(id -u)" \
  -e HERMES_GID="$(id -g)" \
  hermes-agent-ultra gateway run

# Personal profile
docker run -d \
  --name hermes-ultra-personal \
  --restart unless-stopped \
  --network host \
  -v ~/.hermes-ultra-personal:/data \
  -e HERMES_UID="$(id -u)" \
  -e HERMES_GID="$(id -g)" \
  hermes-agent-ultra gateway run
```

Why separate containers over profiles in Docker:

- **Isolation** — each container has its own filesystem, process table, and resource limits. A crash, dependency change, or runaway session in one profile can't affect another.
- **Independent lifecycle** — upgrade, restart, pause, or roll back each agent separately (`docker restart hermes-ultra-work` leaves `hermes-ultra-personal` untouched).
- **Clean port and network separation** — each gateway binds its own host port; there's no risk of cross-talk between chat platforms or API servers.
- **Simpler mental model** — the container *is* the profile. Backups, migrations, and permissions all follow the bind-mounted directory, with no extra `--profile` flags to remember.
- **Avoids concurrent-write risk** — the warning above about never running two gateways against the same data directory still applies to profiles within a single container.

In Docker Compose, this just means declaring one service per profile with distinct `container_name`, `volumes`, and any explicit API-server ports/keys you choose to expose:

```yaml
services:
  hermes-work:
    image: hermes-agent-ultra
    container_name: hermes-ultra-work
    restart: unless-stopped
    network_mode: host
    volumes:
      - ~/.hermes-ultra-work:/data
    environment:
      - HERMES_UID=${HERMES_UID:-10000}
      - HERMES_GID=${HERMES_GID:-10000}
    command: ["gateway", "run"]

  hermes-personal:
    image: hermes-agent-ultra
    container_name: hermes-ultra-personal
    restart: unless-stopped
    network_mode: host
    volumes:
      - ~/.hermes-ultra-personal:/data
    environment:
      - HERMES_UID=${HERMES_UID:-10000}
      - HERMES_GID=${HERMES_GID:-10000}
    command: ["gateway", "run"]
```

## Environment variable forwarding

API keys are read from `/data/.env` inside the container. You can also pass environment variables directly:

```sh
docker run -it --rm \
  -v ~/.hermes-agent-ultra:/data \
  -e ANTHROPIC_API_KEY="sk-ant-..." \
  -e OPENAI_API_KEY="sk-..." \
  hermes-agent-ultra
```

Direct `-e` flags override values from `.env`. This is useful for CI/CD or secrets-manager integrations where you don't want keys on disk.

## Docker Compose example

For persistent deployment with both the gateway and dashboard, this repository's `docker-compose.yml` uses two services over the same image and data volume:

```yaml
services:
  gateway:
    build: .
    image: hermes-agent-ultra
    container_name: hermes-agent-ultra
    restart: unless-stopped
    network_mode: host
    volumes:
      - ~/.hermes-agent-ultra:/data
    environment:
      - HERMES_UID=${HERMES_UID:-10000}
      - HERMES_GID=${HERMES_GID:-10000}
    command: ["gateway", "run"]

  dashboard:
    image: hermes-agent-ultra
    container_name: hermes-agent-ultra-dashboard
    restart: unless-stopped
    network_mode: host
    depends_on:
      - gateway
    volumes:
      - ~/.hermes-agent-ultra:/data
    environment:
      - HERMES_UID=${HERMES_UID:-10000}
      - HERMES_GID=${HERMES_GID:-10000}
    command: ["dashboard", "--host", "127.0.0.1", "--no-open"]
```

Start with `HERMES_UID=$(id -u) HERMES_GID=$(id -g) docker compose up -d` and view logs with `docker compose logs -f`.

## Resource limits

The Hermes container needs moderate resources. Recommended minimums:

| Resource | Minimum | Recommended |
|----------|---------|-------------|
| Memory | 1 GB | 2–4 GB |
| CPU | 1 core | 2 cores |
| Disk (data volume) | 500 MB | 2+ GB (grows with sessions/skills) |

Browser automation (Playwright/Chromium) is the most memory-hungry feature. If you don't need browser tools, 1 GB is sufficient. With browser tools active, allocate at least 2 GB.

Set limits in Docker:

```sh
docker run -d \
  --name hermes-agent-ultra \
  --restart unless-stopped \
  --memory=4g --cpus=2 \
  --network host \
  -v ~/.hermes-agent-ultra:/data \
  hermes-agent-ultra gateway run
```

## What the Dockerfile does

The Ultra image uses a Rust multi-stage build and a slim Debian runtime. It includes:

- The compiled Rust `hermes` binary under `/usr/local/bin`.
- `tini` as PID 1 for signal handling and child reaping.
- `gosu` for dropping root to the configured runtime UID/GID.
- `ca-certificates` for HTTPS provider/API calls.
- MIT license metadata plus `LICENSE` and `NOTICE` under `/usr/share/doc/hermes-agent-ultra`.

The entrypoint script (`docker/entrypoint.sh`) keeps runtime state separated from the immutable Rust image:
- Resolves `HERMES_HOME` to `/data` by default.
- Creates the data directory if it is missing.
- Remaps the `hermes` user/group to `HERMES_UID` / `HERMES_GID` when the container starts as root.
- Recursively repairs ownership only for the mounted data directory when needed.
- Drops privileges with `gosu` before running the requested `hermes` command.

:::warning
Do not override the image entrypoint unless you keep `/usr/local/bin/hermes-entrypoint` in the command chain. The entrypoint drops root privileges to the `hermes` user before gateway state files are created. Starting `hermes gateway run` as root can leave root-owned files in `/data` and break later dashboard or gateway starts.
:::

## Upgrading

Rebuild or pull the updated image and recreate the container. Your data directory is untouched.

```sh
docker build -t hermes-agent-ultra .
docker rm -f hermes-agent-ultra
docker run -d \
  --name hermes-agent-ultra \
  --restart unless-stopped \
  --network host \
  -v ~/.hermes-agent-ultra:/data \
  hermes-agent-ultra gateway run
```

Or with Docker Compose:

```sh
docker compose pull
docker compose up -d
```

## Skills and credential files

When using Docker as the execution environment (not the methods above, but when the agent runs commands inside a Docker sandbox — see [Configuration → Docker Backend](./configuration.md#docker-backend)), Hermes reuses a single long-lived container for all tool calls and automatically bind-mounts the skills directory (`~/.hermes/skills/`) and any credential files declared by skills into that container as read-only volumes. Skill scripts, templates, and references are available inside the sandbox without manual configuration, and because the container persists for the life of the Hermes process, any dependencies you install or files you write stay around for the next tool call.

The same syncing happens for SSH and Modal backends — skills and credential files are uploaded via rsync or the Modal mount API before each command.

## Connecting to local inference servers (vLLM, Ollama, etc.)

When running Hermes in Docker and your inference server (vLLM, Ollama, text-generation-inference, etc.) is also running on the host or in another container, networking requires extra attention.

### Docker Compose (recommended)

Put both services on the same Docker network. This is the most reliable approach:

```yaml
services:
  vllm:
    image: vllm/vllm-openai:latest
    container_name: vllm
    command: >
      --model Qwen/Qwen2.5-7B-Instruct
      --served-model-name my-model
      --host 0.0.0.0
      --port 8000
    ports:
      - "8000:8000"
    networks:
      - hermes-net
    deploy:
      resources:
        reservations:
          devices:
            - capabilities: [gpu]

  hermes:
    image: hermes-agent-ultra
    container_name: hermes-agent-ultra
    restart: unless-stopped
    volumes:
      - ~/.hermes-agent-ultra:/data
    networks:
      - hermes-net
    command: ["gateway", "run"]

networks:
  hermes-net:
    driver: bridge
```

Then in your `~/.hermes-agent-ultra/config.yaml`, use the **container name** as the hostname:

```yaml
model:
  provider: custom
  model: my-model
  base_url: http://vllm:8000/v1
  api_key: "none"
```

:::tip Key points
- Use the **container name** (`vllm`) as the hostname — not `localhost` or `127.0.0.1`, which refer to the Hermes container itself.
- The `model` value must match the `--served-model-name` you passed to vLLM.
- Set `api_key` to any non-empty string (vLLM requires the header but doesn't validate it by default).
- Do **not** include a trailing slash in `base_url`.
:::

### Standalone Docker run (no Compose)

If your inference server runs directly on the host (not in Docker), use `host.docker.internal` on macOS/Windows, or `--network host` on Linux:

**macOS / Windows:**

```sh
docker run -d \
  --name hermes-agent-ultra \
  -v ~/.hermes-agent-ultra:/data \
  hermes-agent-ultra gateway run
```

```yaml
# config.yaml
model:
  provider: custom
  model: my-model
  base_url: http://host.docker.internal:8000/v1
  api_key: "none"
```

**Linux (host networking):**

```sh
docker run -d \
  --name hermes-agent-ultra \
  --network host \
  -v ~/.hermes-agent-ultra:/data \
  hermes-agent-ultra gateway run
```

```yaml
# config.yaml
model:
  provider: custom
  model: my-model
  base_url: http://127.0.0.1:8000/v1
  api_key: "none"
```

:::warning With `--network host`, the `-p` flag is ignored — all container ports are directly exposed on the host.
:::

### Verifying connectivity

From inside the Hermes container, confirm the inference server is reachable:

```sh
docker exec hermes-agent-ultra curl -s http://vllm:8000/v1/models
```

You should see a JSON response listing your served model. If this fails, check:

1. Both containers are on the same Docker network (`docker network inspect hermes-net`)
2. The inference server is listening on `0.0.0.0`, not `127.0.0.1`
3. The port number matches

### Ollama

Ollama works the same way. If Ollama runs on the host, use `host.docker.internal:11434` (macOS/Windows) or `127.0.0.1:11434` (Linux with `--network host`). If Ollama runs in its own container on the same Docker network:

```yaml
model:
  provider: custom
  model: llama3
  base_url: http://ollama:11434/v1
  api_key: "none"
```

## Troubleshooting

### Container exits immediately

Check logs: `docker logs hermes-agent-ultra`. Common causes:
- Missing or invalid `.env` file — run interactively first to complete setup
- Port conflicts if running with exposed ports

### "Permission denied" errors

The container's entrypoint drops privileges to the non-root `hermes` user (UID 10000 by default) via `gosu`. If your host data directory is owned by your user, set `HERMES_UID`/`HERMES_GID` to match instead of recursively changing permissions:

```sh
HERMES_UID="$(id -u)" HERMES_GID="$(id -g)" docker compose up -d
```

### Browser tools not working

If you add browser tooling to a custom image, Chromium/Playwright typically needs shared memory. Add `--shm-size=1g` to that custom Docker run command:

```sh
docker run -d \
  --name hermes-agent-ultra \
  --shm-size=1g \
  -v ~/.hermes-agent-ultra:/data \
  hermes-agent-ultra gateway run
```

### Gateway not reconnecting after network issues

The `--restart unless-stopped` flag handles most transient failures. If the gateway is stuck, restart the container:

```sh
docker restart hermes-agent-ultra
```

### Checking container health

```sh
docker logs --tail 50 hermes-agent-ultra       # Recent logs
docker run -it --rm hermes-agent-ultra version # Verify version
docker stats hermes-agent-ultra                # Resource usage
```
