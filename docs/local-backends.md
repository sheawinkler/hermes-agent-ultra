# Local/Self-Host Backends

Hermes Agent Ultra supports OpenAI-compatible local or private inference endpoints directly.

## Provider IDs

- `ollama-local`
- `llama-cpp`
- `vllm` (aliases: `ollvm`, `llvm`)
- `mlx` (aliases: `mlx-lm`, `apple-mlx`, `vmlx`, `omlx`, `mlx-vlm`)
- `apple-ane`
- `sglang`
- `tgi`
- `lmstudio` (aliases: `lm-studio`, `lm_studio`)
- `lmdeploy` (aliases: `lm-deploy`, `lm_deploy`)
- `localai` (aliases: `local-ai`, `local_ai`)
- `koboldcpp` (aliases: `kobold-cpp`, `kobold`)
- `text-generation-webui` (aliases: `oobabooga`, `textgen-webui`)
- `tabbyapi` (aliases: `tabby-api`, `exllama`, `exllamav2`)

`llamafile` is treated as a `llama-cpp` server variant.

## Endpoint env overrides

- `OLLAMA_BASE_URL` (default `http://127.0.0.1:11434/v1`)
- `LLAMA_CPP_BASE_URL` (default `http://127.0.0.1:8080/v1`)
- `VLLM_BASE_URL` (default `http://127.0.0.1:8000/v1`)
- `MLX_BASE_URL` (default `http://127.0.0.1:8080/v1`)
- `APPLE_ANE_BASE_URL` (default `http://127.0.0.1:8081/v1`)
- `SGLANG_BASE_URL` (default `http://127.0.0.1:30000/v1`)
- `TGI_BASE_URL` (default `http://127.0.0.1:8082/v1`)
- `LMSTUDIO_BASE_URL` (default `http://127.0.0.1:1234/v1`)
- `LMDEPLOY_BASE_URL` (default `http://127.0.0.1:23333/v1`)
- `LOCALAI_BASE_URL` (default `http://127.0.0.1:8080/v1`)
- `KOBOLDCPP_BASE_URL` (default `http://127.0.0.1:5001/v1`)
- `TEXT_GENERATION_WEBUI_BASE_URL` (default `http://127.0.0.1:5000/v1`)
- `TABBYAPI_BASE_URL` (default `http://127.0.0.1:5000/v1`)

Optional API-key env vars are also supported if your server enforces auth:

- `OLLAMA_LOCAL_API_KEY`, `LLAMA_CPP_API_KEY`, `VLLM_API_KEY`, `MLX_API_KEY`,
  `APPLE_ANE_API_KEY`, `SGLANG_API_KEY`, `TGI_API_KEY`, `LMSTUDIO_API_KEY`,
  `LMDEPLOY_API_KEY`, `LOCALAI_API_KEY`, `KOBOLDCPP_API_KEY`,
  `TEXT_GENERATION_WEBUI_API_KEY`, `TABBYAPI_API_KEY`

## Setup flow

Run:

```bash
hermes-ultra setup
```

Pick a local provider in the provider menu. Local providers do not require OAuth and do not require an API key by default. For no-key localhost/private endpoints, Hermes uses a local no-key marker internally and does not add an authorization bearer header to OpenAI-compatible requests.

## Model selection

Use:

```bash
/model
```

Hermes will show curated model suggestions and attempt live model discovery via `GET /v1/models` where the backend exposes it.

For provider contract checks and catalog/auth diagnostics:

```bash
/model harness
/model harness openrouter
/model harness huggingface:Qwen/Qwen3.5-397B-A17B
```

## Hugging Face live catalog fusion

Hermes Ultra now treats Hugging Face as a first-class dynamic catalog:

- curated compatibility picks are always included first
- live router models are appended from `HF_BASE_URL/models` (default `https://router.huggingface.co/v1/models`)
- models.dev-discovered agentic entries are appended after live results

Environment controls:

- `HF_TOKEN` (preferred) or `HUGGINGFACE_API_KEY`
- `HF_BASE_URL` (optional; defaults to HF router endpoint)
- `HERMES_HF_CATALOG_DISABLE_LIVE=1` to disable live catalog fetch
- `HERMES_HF_CATALOG_LIMIT=120` to cap appended live entries

## Backend best-practice overlays (vLLM, mistral.rs, and local servers)

Use backend overlays to quickly apply performance defaults inspired by leading local runtimes:

```bash
/model backend list
/model backend show vllm throughput
/model backend apply vllm reliability
```

Current profiles include:

- `vllm` (`balanced`, `throughput`, `reliability`)
- `llama-cpp` (`balanced`)
- `mlx` (`balanced`)
- `apple-ane` (`balanced`)
- `sglang` (`balanced`)
- `tgi` (`balanced`)
- `lmstudio` (`balanced`)
- `lmdeploy` (`balanced`)
- `localai` (`balanced`)
- `koboldcpp` (`balanced`)
- `text-generation-webui` (`balanced`)
- `tabbyapi` (`balanced`)
- `mistral-rs` (`balanced` guidance profile)

Applying a profile:

- sets process env overrides immediately for the active runtime
- writes a persisted profile file under `~/.hermes-agent-ultra/runtime/backend_profiles/*.env`
- forces model runtime refresh when applying to the currently active provider

## Async background job observability

Background tasks can now be inspected without leaving the TUI:

```bash
/background status
/background tail <job-id> 120
```

`/queue status` now shows the same enriched background snapshot.

## ContextLattice embedding diagnostics

Use:

```bash
/graph embeddings
```

This probes:

- `GET /health`
- `GET /telemetry/embeddings` (when available)
- fallback telemetry from `GET /telemetry/recall`

The diagnostics are also included in `/graph status`.

## Health checks

Run:

```bash
hermes-ultra doctor --deep
```

Doctor prints optional reachability checks for every provider-runtime local backend endpoint listed above.
