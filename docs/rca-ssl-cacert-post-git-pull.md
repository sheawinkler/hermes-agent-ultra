# RCA: SSL CA Cert Bundle After Update

Status: upstream Python RCA retained as an Ultra diagnostic reference.

Upstream Hermes added a guard for corrupted Python CA bundle state after update
or dependency refresh. Hermes Ultra's Rust runtime uses native Rust TLS stacks
(`rustls`/webpki roots on supported HTTP clients), so normal provider, gateway,
and web requests do not depend on Python `certifi`.

The failure can still matter for reference-only Python plugin trees or local
helper surfaces that an operator runs manually. If a Python plugin fails with a
low-level TLS error after an update, check these variables first:

- `HERMES_CA_BUNDLE`
- `SSL_CERT_FILE`
- `REQUESTS_CA_BUNDLE`
- `CURL_CA_BUNDLE`

Unset stale variables or point them at a real PEM bundle. For Python helper
environments, reinstall the affected client dependencies:

```bash
python -m pip install --force-reinstall certifi requests httpx
```

This document is intentionally diagnostic only. Rust runtime TLS failures should
be debugged through the Rust client error and certificate store, not by adding a
Python CA-bundle guard to the runtime.

