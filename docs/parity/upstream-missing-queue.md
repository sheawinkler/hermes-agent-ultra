# Upstream Missing Patch Queue

Generated: `2026-05-12T08:25:11.445096+00:00`

- Range: `main..upstream/main`; total commits tracked: `2538`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 1018 |
| #21 | GPAR-02 skills parity | 82 |
| #22 | GPAR-03 UX parity | 486 |
| #23 | GPAR-04 gateway/plugin-memory parity | 207 |
| #24 | GPAR-05 environments+parsers+benchmarks | 11 |
| #25 | GPAR-06 packaging/docs/install parity | 37 |
| #26 | GPAR-07 upstream queue backfill | 697 |

| Disposition | Commit Count |
| --- | ---: |
| mirrored | 64 |
| pending | 114 |
| ported | 60 |
| superseded | 2300 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `42e166c7ea2e` | #20 | refactor(docker): drop manual @hermes/ink build, rely on esbuild bundle |
| `401aadb5b892` | #26 | docs(security): rewrite policy around OS-level isolation as the boundary |
| `0d1cbc2dda28` | #26 | changes from feedback |
| `395dbcc873c8` | #20 | feat(browser): add Lightpanda engine support with automatic Chrome fallback |
| `3ebdd26449dc` | #20 | fix(browser): surface Lightpanda Chrome fallback warnings |
| `d78c34928fe9` | #22 | feat(tui): collapsible sections in startup banner (skills, system prompt, MCP) |
| `043a118d4128` | #25 | fix: harden install.sh against inherited Python env leakage |
| `17687911b7c5` | #26 | fix(kanban): reset code element background inside board |
| `b62a82e0c3fb` | #22 | docs: pluggable surfaces coverage — model-provider guide, full plugin map, opt-in fix (#20749) |
| `9627ee70e57a` | #25 | feat(ci): add typecheck (warnings only in CI) |
| `ad7aad251c60` | #21 | feat(skills/linear): add Documents support + Python helper script (#20752) |
| `94016dd1aa7e` | #22 | docs+skill: add searxng-search optional skill and documentation |
| `f4031df05dd4` | #25 | ci(docker): don't cancel overlapping builds, guard :latest |
| `04cf4788ccc0` | #22 | fix(tui): restore voice push-to-talk parity (#20897) |
| `65c762b2e83e` | #20 | fix(tui): preserve session when switching personality |
| `bf843adf05b8` | #20 | feat(gateway): opt-in cleanup of temporary progress bubbles (#21186) |
| `699c770e5c06` | #25 | docs(readme): drop misleading RL install-extras claim, defer to CONTRIBUTING |
| `36ad97337a4a` | #26 | fix(kanban): treat dashboard event-stream cancellation as normal shutdown |
| `61d9e3366d65` | #26 | fix(model_tools): log plugin hook exceptions instead of silently swallowing them |
| `647f95b4224c` | #26 | docs(contributing): align tool discovery and test runner with AGENTS.md |
| `b9f1ac8c1022` | #26 | fix(kanban): make dashboard board pin authoritative over server current file (#21230) |
| `6e46f99e7e8e` | #20 | fix(tui): surface backend error as visible text when final_response is empty (#21245) |
| `84287b0de8dd` | #20 | fix(docker): refuse root gateway runs in official image |
| `2f2f654486f9` | #25 | fix: add dashboard to CLI help epilogue and Docker CI smoke test |
| `498c01406fce` | #25 | fix(docker): chown runtime node_modules trees to hermes user (#18800) |
| `b93c9f639381` | #26 | feat(kanban): convert inline-create title input to multiline textarea |
| `fa582749e165` | #26 | fix(kanban): restore Enter=submit, Shift+Enter=newline in inline-create textarea |
| `ec9d0e26d4ed` | #20 | fix(tui): render structured content on resume |
| `76d2dcdc8e10` | #26 | fix(kanban): make code/pre styling theme-immune across all themes (#21086) (#21247) |
| `44cd79e798e4` | #22 | feat(plugins/google_chat): Google Chat platform adapter as a bundled plugin |
| `62c2f5d8d2a6` | #20 | fix(mcp): coerce numeric tool args defensively |
| `ff0985323509` | #25 | docs(readme): prefer .venv to match AGENTS.md and scripts/run_tests.sh (#21334) |
| `162ad3dd1624` | #20 | fix(kanban): filter dashboard board by selected tenant |
| `498bfc7bc12a` | #26 | chore: release v0.13.0 (2026.5.7) (#21406) |
| `733e297b8a5c` | #20 | fix(acp): inline file attachment resources |
| `7e2af0c2e872` | #20 | feat(acp): pass image file attachments through as image_url parts |
| `da18fd084a0b` | #25 | fix: strengthen termux install network prerequisites |
| `dc5ef1ac8ed9` | #25 | fix: add termux-all install profile and safe fallbacks |
| `24d48ffb8294` | #20 | feat(kanban): add `specify` — auxiliary LLM fleshes out triage tasks (#21435) |
| `d87c7b99e2a4` | #22 | fix(analytics): prevent silent token loss and add Claude 4.5–4.7 pricing (#21455) |
| `292f4683667e` | #20 | fix(mcp): unwrap platforms key in channels_list |
| `c80fa728bd84` | #25 | fix(installer): set UV_NO_CONFIG=1 to avoid permission denied under sudo -u |
| `7d66d30d774e` | #26 | feat(kanban): add tooltips and docs link across dashboard (#21541) |
| `81928f03ab58` | #20 | refactor(gmi): move User-Agent to profile.default_headers |
| `83c23e88617c` | #21 | fix(google-workspace): cleanup for --check-live salvage |
| `5643c2979013` | #26 | feat(docker): bootstrap auth.json from env on first boot |
| `850413f1203f` | #22 | feat(computer-use): cua-driver backend, universal any-model schema |
| `07bbd9333708` | #20 | feat(teams-pipeline): add plugin runtime and operator cli |
| `242da9db965c` | #22 | docs(teams-pipeline): cron renewal recipe, sidebar wiring, skill rewrite |
| `9de893e3b078` | #22 | feat(windows): close native-Windows install gaps — crash-free startup, UTF-8 stdio, tzdata dep, docs |
| `b7fe7ed7bd17` | #25 | feat(windows-install): bundle portable MinGit instead of relying on winget |
| `e93bfc6c93bf` | #20 | feat(windows): close remaining POSIX-only landmines — TUI crash, kanban waitpid, AF_UNIX sandbox, /bin/bash, npm .cmd shims, cwd tracking, detach flags |
| `d94fb47717eb` | #20 | hermes_bootstrap: Windows-only UTF-8 stdio shim for all entry points |
| `cbce5e93fcb9` | #24 | codebase: add encoding='utf-8' to all bare open() calls (PLW1514) |
| `b63f9645f08a` | #21 | docs: add Windows-Specific Quirks section to hermes-agent skill + keystroke diagnostic |
| `98db898c0bd4` | #21 | feat(skills): declare platforms frontmatter for all 79 undeclared built-in skills |
| `324567c93662` | #23 | fix(windows): os.kill(pid, 0) is NOT a no-op on Windows — route through new _pid_exists helper |
| `cc38282b04d9` | #23 | feat(cross-platform): psutil for PID/process management + Windows footgun checker |
| `d3120aeab064` | #25 | ci(lint): add blocking ruff-check + windows-footguns jobs to lint.yml |
| `26bac67ef90d` | #20 | fix(entry-points): guard hermes_bootstrap import so partial updates don't brick hermes (#22091) |
| `bf80508d6566` | #25 | ci: split docker-publish into per-arch native runners |
| `afc186fa4eed` | #25 | docker: split python dep install into cached layer above COPY . . |
| `758c40135f0f` | #25 | ci: add blocking uv.lock check |
| `93679ef27d74` | #25 | ci: run docker build on PRs + smoke test arm64 |
| `7a4d5c123a29` | #22 | docs(windows): label native Windows support as early beta (#22115) |
| `2a7047c2ed42` | #23 | fix(sqlite): fall back to journal_mode=DELETE on NFS/SMB/FUSE (#22043) |
| `7330183d087f` | #26 | fix(model_tools): log warnings for failed JSON-array coercion |
| `4a1840e68350` | #24 | fix(async): replace get_event_loop() with get_running_loop() in async contexts |
| `93e25ceb1326` | #20 | feat(plugins): add standalone_sender_fn for out-of-process cron delivery |
| `1ac8deb3caa3` | #23 | feat(gateway): stream Telegram edits safely |
| `55f518e5216a` | #20 | feat(gateway): add Telegram guest mention mode |
| `883e11f0a09a` | #20 | fix(openrouter): add x-grok-conv-id header for Grok models to improve prompt cache hit rates (carve-out of #22708) |
| `840ebe063eea` | #20 | fix: make session search initialize session db |
| `c7f0aab9497b` | #20 | feat(openrouter): wire Pareto Code router with min_coding_score knob (#22838) |
| `058c50816c70` | #20 | fix(session): route OR-combined short CJK tokens to LIKE fallback (#20494) |
| `c179bdab3c5f` | #25 | fix(install): also patch psutil on Termux fresh-install path |
| `85383c636309` | #20 | fix(cli): preserve config comments on setting writes |
| `ded194eb6aca` | #21 | chore(skills): move heavy training skills + outlines to optional-skills (#22912) |
| `d04a0b81ee7c` | #21 | docs(skills): clarify kanban fan-out decomposition |
| `8954537f956b` | #20 | fix(kanban): request default board explicitly (#21819) |
| `236cbe16b62c` | #22 | feat(kanban): add orchestrator board tools |
| `50f9fee988b6` | #22 | feat(gateway): add LINE Messaging API platform plugin (#23197) |
| `ae4b09ce1073` | #20 | test(security): broaden plugin API auth coverage + correct stale docstring |
| `5aa755e4e63c` | #22 | feat(plugins): run any LLM call from inside a plugin via ctx.llm (#23194) |
| `c39168453d01` | #22 | feat(i18n): localize all gateway commands + web dashboard, add 8 new locales (16 total) (#22914) |
| `061a18300837` | #26 | fix(kanban): guard task_age against corrupt created_at values like '%s' |
| `b308dd7d750c` | #26 | fix(kanban): preserve assignee casing in dashboard |
| `0e0ddaac8fa0` | #20 | fix(kanban-dashboard): tone down completed-run metadata panel (#19548) |
| `a91e5a87594b` | #20 | feat(kanban-dashboard): native <details> collapse + skip empty metadata |
| `6e5c49bdc40d` | #22 | refactor(kanban-orchestrator): drop hardcoded specialist roster, add Step-0 profile discovery |
| `878611a79dfa` | #20 | feat(session): add /handoff command for cross-platform session transfer |
| `00ce5f04d9cf` | #23 | feat(session): make /handoff actually transfer the session live |
| `6062c24fd1c2` | #25 | ci: skip lint comment on fork PRs |
| `404640a2b752` | #20 | feat(goals): /goal checklist + /subgoal user controls (#23456) |
| `ae83a54be450` | #22 | docs(kanban): worker lane contract page + review-required convention |
| `da2ed478b505` | #26 | fix(achievements): inject Authorization header in plugin API calls |
| `80bb5f294755` | #26 | fix(achievements): use canonical X-Hermes-Session-Token header |
| `518d37f6af49` | #26 | feat(kanban): add reclaim_first support to bulk reassign endpoint |
| `0ea234e0932f` | #20 | feat(kanban): dashboard batch QOL upgrade |
| `98c499b235e4` | #26 | kanban dashboard: fix batch QOL oracle blockers |

