---
name: cloudflare-temporary-deploy
description: Deploy a Worker live, no account, via wrangler --temporary.
version: 1.0.0
author: Hermes Agent
license: MIT
platforms: [linux, macos, windows]
metadata:
  hermes:
    tags: [cloudflare, workers, wrangler, deploy, temporary, agent, serverless, web-development]
    category: web-development
---

# Cloudflare Temporary Deploy Skill

Deploy a Cloudflare Worker to a live `workers.dev` URL with zero account setup, using `wrangler deploy --temporary`. Cloudflare provisions a throwaway account, deploys, and prints a claim URL valid for 60 minutes; unclaimed accounts auto-delete. This gives an agent a tight write -> deploy -> verify loop without OAuth, signup, or token copy-paste.

This skill does not cover production deploys. Use `wrangler login` plus a permanent account for those. It also does not cover non-Worker Cloudflare products beyond the temporary-account limits below.

## When to Use

Load this skill when the user wants to:

- Ship agent-written code to a live URL without first creating a Cloudflare account.
- Iterate in a background/autonomous session where browser OAuth would be a hard stop.
- Prototype or evaluate Workers quickly with a throwaway, claimable target.
- Build a self-verifying deploy loop: deploy, `curl` the live URL, confirm output matches the code, redeploy.

## When NOT to Use

- Production or CI/CD: use a permanent account with `wrangler login` or `CLOUDFLARE_API_TOKEN`. `--temporary` errors out if credentials are present.
- Wrangler is already authenticated: `--temporary` returns an error by design. Run `wrangler logout` first only if the user explicitly wants a throwaway deploy.
- Long-lived hosting: temporary deployments are deleted after 60 minutes unless claimed.

## Prerequisites

- Wrangler 4.102.0 or later. This is the version that introduced `--temporary`. Verify with `npx wrangler@latest --version`.
- Node 18+ with `npx`, `npm`, `yarn`, or `pnpm`. No global install is required.
- No Cloudflare credentials present. `--temporary` only works when Wrangler is unauthenticated: no OAuth login, no `CLOUDFLARE_API_TOKEN` / `CLOUDFLARE_API_KEY` env var, and no cached `~/.wrangler` or `~/.config/.wrangler` OAuth session in the shell you use.
- Network egress to `cloudflare.com` and `workers.dev`.
- Using `--temporary` accepts Cloudflare's Terms of Service and Privacy Policy.

## How to Run

Use the terminal tool for every step. Always pin Wrangler with `wrangler@latest` or a version `>=4.102.0` so you do not accidentally run an old global binary that lacks `--temporary`.

1. Scaffold a minimal Worker if the project does not already exist.

   `wrangler.jsonc`:

   ```jsonc
   {
     "name": "hello-agent",
     "main": "src/index.ts",
     "compatibility_date": "2025-01-01"
   }
   ```

   `src/index.ts`:

   ```typescript
   export default {
     async fetch(): Promise<Response> {
       return new Response("hello cloudflare");
     },
   };
   ```

2. Deploy with `--temporary` from the project directory.

   ```bash
   npx wrangler@latest deploy --temporary
   ```

   The proof-of-work check adds a short automatic delay. On success, Wrangler prints an `Account: <name> (created)` or `(reused)` line, a `Claim URL`, and the live `https://<worker>.<account>.workers.dev` URL.

3. Parse the URLs from deploy output with Hermes' Rust parser.

   ```bash
   npx wrangler@latest deploy --temporary 2>&1 | hermes cloudflare parse-temporary-deploy-output
   ```

   The parser prints JSON with `live_url`, `claim_url`, `account`, `account_state`, `expires_minutes`, and `deployed`. It exits non-zero if no live `workers.dev` URL is present, so scripts can branch on failure.

4. Verify the deploy is actually live. Do not trust the deploy log alone.

   ```bash
   curl -sS <live_url>
   ```

   Confirm the body matches what the Worker code returns.

5. Iterate. Edit the Worker, then redeploy with the same command. Within the 60-minute window Wrangler reuses the cached temporary account, so the URL stays stable. `curl` again to confirm the change.

6. Hand the claim URL to the user. Tell them to open it within 60 minutes to keep the deployment and resources. If they do not claim it, everything auto-deletes. Treat the claim URL as a secret because it grants ownership of the temporary account.

## Quick Reference

| Step | Command |
|---|---|
| Check version | `npx wrangler@latest --version` |
| Deploy without account | `npx wrangler@latest deploy --temporary` |
| Deploy and parse URLs | `npx wrangler@latest deploy --temporary 2>&1 \| hermes cloudflare parse-temporary-deploy-output` |
| Parser self-test | `hermes cloudflare --selftest` |
| Verify live URL | `curl -sS <live_url>` |
| Clear cached temporary account | `npx wrangler@latest logout` |

## Temporary Account Product Limits

| Product | Limit on a temporary account |
|---|---|
| Workers | Deploys to `workers.dev` |
| Static Assets | Up to 1,000 files, 5 MiB each |
| KV | Allowed |
| D1 | 1 database, 100 MB per DB / 100 MB total |
| Durable Objects | Allowed |
| Hyperdrive | 2 configs, 10 connections |
| Queues | Up to 10 |
| SSL/TLS certs | Allowed |

## Pitfalls

- `--temporary` may be hidden from `wrangler deploy --help` and is not a global flag. Do not conclude the flag is missing only because help omits it. Check the version instead.
- A stale globally-installed `wrangler` older than 4.102.0 lacks the flag. Prefer `npx wrangler@latest` or a pinned version at or above 4.102.0.
- If `wrangler login` was run, or if `CLOUDFLARE_API_TOKEN` / `CLOUDFLARE_API_KEY` is set, `--temporary` errors. Either unset the variable for this shell or log out, but never strip a user's real credentials without telling them.
- Creating temporary accounts too fast can rate-limit. Reuse the cached account within the 60-minute window instead of forcing a new one.
- The 60-minute expiry is hard. If the deploy must outlive an hour, the user must claim it.
- `curl` can briefly serve the old body after a redeploy. A `(reused)` account line plus a new version ID can confirm deployment while edge caches catch up; re-curl or add a cache-busting query string.
- Do not log the claim URL into shared transcripts as a harmless link. It is credential-equivalent.

## Verification

- `npx wrangler@latest --version` returns 4.102.0 or newer.
- `npx wrangler@latest deploy --temporary` prints a `workers.dev` live URL and a `claim-preview?claimToken=` claim URL.
- `npx wrangler@latest deploy --temporary 2>&1 | hermes cloudflare parse-temporary-deploy-output` prints structured JSON and exits zero.
- `curl -sS <live_url>` returns the exact body the Worker code produces.
- A second deploy reports `Account: <name> (reused)` and the live URL is unchanged.
- The parser self-test passes with `hermes cloudflare --selftest`.
