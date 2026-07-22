# Kimi K3 coding-use decision

Baseline commit: `c002f8c4efe475c89c9f14d3995d44e6a5df5b27`
Decision date: 2026-07-17
Pre-use verdict: no-go for this program unless the operator later supplies an approved zero-cost or funded route.

## Verified facts

- Official model ID is `kimi-k3`.
- Official API inference is paid; no unlimited free coding API was found.
- K3 uses top-level `reasoning_effort: "max"` and must not receive the K2 `thinking` field.
- Local Hermes defaults still target deprecated `kimi-k2-turbo-preview` and can emit both `thinking` and `reasoning_effort`.
- Upstream improves K2 mutual exclusion/discovery but is not yet a complete K3-native profile.
- Only non-generation authentication/model-list probes were performed; no billable completion was issued.

## Required packet before future use

1. Port the upstream thinking/effort mutual-exclusion behavior.
2. Add an explicit `kimi-k3` request profile with `reasoning_effort=max` and no K2 `thinking` field.
3. Replace deprecated defaults with a reviewed model policy.
4. Extend Kimi/Moonshot model detection and tool-schema handling.
5. Prove request bodies with unit tests and use only a non-generation `/models` probe until the operator explicitly approves paid inference.

## Remaining risk

Catalog/profile presence is not runtime proof. Do not route coding work to Kimi K3 under the current local profile and do not make a paid API request implicitly.
