---
name: touchdesigner-mcp
description: Control and debug TouchDesigner projects through MCP-backed tooling for scene graph edits, operator introspection, timeline automation, and audio-reactive visuals.
version: 1.0.0
author: Hermes Agent Ultra
license: MIT
metadata:
  hermes:
    tags: [touchdesigner, mcp, realtime, visual, audio-reactive, op, glsl]
    category: creative
---

# touchdesigner-mcp

Use this skill when users want production-style TouchDesigner assistance that
is agent-friendly and terminal-repeatable.

## Use Cases

- Build or modify node networks from prompts
- Diagnose broken TOP/CHOP/DAT/COMP operator chains
- Automate timeline/cue playback and scene state transitions
- Drive reactive visuals from audio or event streams
- Generate reproducible patch notes for `.toe` workflows

## Workflow

1. Discover available MCP commands for TouchDesigner runtime control.
2. Snapshot current network state (operators, links, key params).
3. Apply minimal graph edits in small batches.
4. Validate output (render path, FPS, operator errors, cue timing).
5. Emit structured changelog + rollback instructions.

## Guardrails

- Keep edits additive unless user explicitly requests destructive rewrites.
- Prefer parameter-level changes before deleting operators.
- Preserve naming conventions on critical nodes (`in_*`, `fx_*`, `out_*`).
- When performance regresses, prioritize cook-time hotspots and texture size.

## Suggested Prompt Add-ons

- "Optimize for 60fps at 1080p."
- "Keep existing scene naming and transition cues."
- "Use deterministic naming and provide rollback steps."

## References

- `references/mcp-tools.md`
