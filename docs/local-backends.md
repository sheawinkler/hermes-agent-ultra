# Local/Self-Host Backends

Hermes Agent Ultra supports OpenAI-compatible local or private inference endpoints directly.

## Provider IDs

- `ollama-local`
- `llama-cpp`
- `vllm` (aliases: `ollvm`, `llvm`)
- `mlx`
- `apple-ane`
- `sglang`
- `tgi`

## Endpoint env overrides

- `OLLAMA_BASE_URL` (default `http://127.0.0.1:11434/v1`)
- `LLAMA_CPP_BASE_URL` (default `http://127.0.0.1:8080/v1`)
- `VLLM_BASE_URL` (default `http://127.0.0.1:8000/v1`)
- `MLX_BASE_URL` (default `http://127.0.0.1:8080/v1`)
- `APPLE_ANE_BASE_URL` (default `http://127.0.0.1:8081/v1`)
- `SGLANG_BASE_URL` (default `http://127.0.0.1:30000/v1`)
- `TGI_BASE_URL` (default `http://127.0.0.1:8082/v1`)

Optional API-key env vars are also supported if your server enforces auth:

- `OLLAMA_LOCAL_API_KEY`, `LLAMA_CPP_API_KEY`, `VLLM_API_KEY`, `MLX_API_KEY`,
  `APPLE_ANE_API_KEY`, `SGLANG_API_KEY`, `TGI_API_KEY`

## Setup flow

Run:

```bash
hermes-ultra setup
```

Pick a local provider in the provider menu. Local providers do not require OAuth and do not require an API key by default.

## Model selection

Use:

```bash
/model
```

Hermes will show curated model suggestions and attempt live model discovery via `GET /v1/models` where the backend exposes it.

## Health checks

Run:

```bash
hermes-ultra doctor --deep
```

Doctor prints optional reachability checks for each local backend endpoint.
