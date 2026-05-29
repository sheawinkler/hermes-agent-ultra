# Network Egress Isolation for Docker Deployments

The default Docker Compose profile runs Hermes Agent Ultra with
`network_mode: host`. That is convenient for local gateway and dashboard
development, but it gives commands executed by the runtime the host network
surface. For production-like deployments, segment the container network so
Hermes can reach only the services it needs.

This is primarily defense in depth against prompt injection that tries to
exfiltrate data through terminal tools such as `curl`, `wget`, or raw HTTP
clients.

## Threat Model

The terminal backend is the primary execution boundary. If a malicious command
does execute inside the container, network egress isolation adds a second
control: the command cannot reach arbitrary external endpoints unless that
traffic is explicitly routed through an allowlisted path.

## Architecture

```text
Docker network: internal, no internet

  hermes-agent-ultra  <-->  dashboard
          |
          v
      gateway
          |
          v

Docker network: egress, internet-capable

    egress-proxy  --->  allowlisted hosts
```

Use two Docker networks:

- `internal`: bridge network with `internal: true`; no default internet route.
- `egress`: bridge network with outbound access; attach only services that need
  platform APIs, model APIs, or update endpoints.

The gateway can be dual-homed so it can receive platform traffic and still talk
to the internal Hermes runtime.

## Compose Override

Create `docker-compose.override.yml` next to the repository compose file:

```yaml
networks:
  internal:
    driver: bridge
    internal: true
  egress:
    driver: bridge

services:
  gateway:
    network_mode: ""
    networks:
      - internal
      - egress
    ports:
      - "127.0.0.1:9119:9119"

  dashboard:
    network_mode: ""
    networks:
      - internal
```

## Egress Proxy

For stricter control, route outbound traffic through an HTTP proxy with a host
allowlist:

```yaml
services:
  gateway:
    network_mode: ""
    networks:
      - internal
      - egress
    environment:
      - HTTP_PROXY=http://egress-proxy:3128
      - HTTPS_PROXY=http://egress-proxy:3128
      - NO_PROXY=localhost,127.0.0.1,hermes-agent-ultra-dashboard

  dashboard:
    network_mode: ""
    networks:
      - internal

  egress-proxy:
    image: ubuntu/squid:6.10-24.04_edge
    networks:
      - egress
    volumes:
      - ./config/squid-allowlist.conf:/etc/squid/conf.d/allowlist.conf:ro
    restart: unless-stopped
```

Example `config/squid-allowlist.conf`:

```text
acl allowed_hosts dstdomain api.openai.com
acl allowed_hosts dstdomain api.anthropic.com
acl allowed_hosts dstdomain openrouter.ai
acl allowed_hosts dstdomain generativelanguage.googleapis.com
acl allowed_hosts dstdomain api.telegram.org
acl allowed_hosts dstdomain api.github.com
acl allowed_hosts dstdomain discord.com

http_access allow CONNECT allowed_hosts
http_access deny all
```

Adjust the allowlist to match enabled model providers, memory providers, and
messaging platforms.

## Validation

After starting the stack, verify that internet access is blocked where expected
and internal service traffic still works:

```bash
docker --context orbstack compose exec gateway \
  curl -sf --max-time 5 https://example.com \
  && echo "FAIL: egress not blocked" || echo "OK: egress blocked"

docker --context orbstack compose exec gateway \
  curl -sf --max-time 5 http://hermes-agent-ultra-dashboard:9119/health \
  && echo "OK: internal reachable" || echo "FAIL: dashboard unreachable"

docker --context orbstack compose exec gateway \
  curl -sf --max-time 5 --proxy http://egress-proxy:3128 https://api.openai.com/v1/models \
  && echo "OK: allowlisted egress works" || echo "FAIL: allowlisted egress blocked"
```

## Limitations

- DNS resolution can still leak low-value metadata unless you also provide a
  restricted resolver for the internal network.
- This isolates container networking, not the local host terminal backend. For
  stronger command isolation, combine this with a sandboxed execution backend.
- Platform adapters need outbound access to their APIs. Add new adapter hosts to
  the proxy allowlist when enabling those adapters.

## Related

- [docker-compose.yml](../../docker-compose.yml)
- [Local backends](../local-backends.md)
- [Parity compatibility policy](../parity/compatibility-policy.md)
