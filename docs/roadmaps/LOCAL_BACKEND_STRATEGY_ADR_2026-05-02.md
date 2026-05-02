# ADR: Local Backend Strategy (Ollama/llama.cpp/vLLM/MLX/ANE)

## Status
Accepted (2026-05-02)

## Context
Hermes Agent Ultra needs broad multi-provider support for cloud + self-host inference with minimal operator friction. Users requested first-class local backend support and asked whether we should build a native Rust inference backend now.

## Decision
1. Treat OpenAI-compatible local inference servers as first-class providers in Hermes Ultra.
2. Support these provider families directly in setup/model/runtime:
   - `ollama-local`
   - `llama-cpp`
   - `vllm` (plus aliases `ollvm`/`llvm`)
   - `mlx`
   - `apple-ane`
   - `sglang`
   - `tgi`
3. Allow local/private endpoint operation without mandatory API keys.
4. Keep runtime Rust-first in orchestration and tooling, while inference execution is delegated to external specialized engines.
5. Defer a full native Rust inference server to a separate phase with explicit performance gates.

## Rationale
- Existing local backends already provide mature model execution, batching, quantization, and GPU/ANE integrations.
- Hermes Ultra value is orchestration quality, policy/safety, memory, tooling, and operator UX.
- Building a full in-process inference engine now would add substantial maintenance risk and slow parity/upkeep velocity.

## Rust-native backend note
Rust-native projects exist in the ecosystem (for example around Candle/mistral.rs/llama-rs style stacks), but a production replacement for all current backend capabilities is not yet a short-path objective for Hermes Ultra.

Decision rule:
- keep external backends as default execution layer now;
- revisit native backend only if a measurable reliability/latency/cost advantage is demonstrated under Hermes Ultra workloads.

## Follow-up criteria for future native backend evaluation
- P95 latency improvement >= 20% on representative tool-calling workloads
- No regression in tool-calling correctness and structured output reliability
- Operational footprint stays compatible with current setup/doctor/deploy workflows
- Security and provenance controls remain at or above current levels
