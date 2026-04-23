---
name: page-agent
description: Embed alibaba/page-agent into your web application as an in-page GUI agent for natural-language UI control. Use when a developer wants AI actions inside their own app UI, not external browser automation.
version: 1.0.0
author: Hermes Agent Ultra
license: MIT
metadata:
  hermes:
    tags: [web, javascript, page-agent, gui, copilot, embedded-agent]
    category: web-development
---

# page-agent

`alibaba/page-agent` is an in-page TypeScript agent that executes natural language actions against your DOM.

## Use this for

- Embedding AI copilots in SaaS/admin/B2B products.
- Adding natural-language interaction to existing web interfaces.
- Rapid demo integrations via script tag or npm package.

## Do not use this for

- Server-side browser automation or remote browsing workflows.
- Screenshot-vision workflows requiring multimodal interpretation.

## Quick paths

1. CDN demo (`<script ...page-agent.demo.js>`).
2. npm integration (`npm install page-agent`) with your own API endpoint.
3. Source workflow (`git clone`, `npm run dev:demo`) for contributor-level customization.

## Provider compatibility

Use OpenAI-compatible endpoints (`baseURL`, `apiKey`, `model`) such as Qwen, OpenAI, OpenRouter, or Ollama-compatible gateways.

## Security baseline

Do not hardcode production API keys in frontend bundles; proxy calls through your backend.
