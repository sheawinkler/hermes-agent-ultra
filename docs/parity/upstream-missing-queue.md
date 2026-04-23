# Upstream Missing Patch Queue

Generated: `2026-04-23T20:06:09.016533+00:00`

- Range: `main..upstream/main`; total commits tracked: `74`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 16 |
| #22 | GPAR-03 UX parity | 20 |
| #23 | GPAR-04 gateway/plugin-memory parity | 4 |
| #26 | GPAR-07 upstream queue backfill | 34 |

| Disposition | Commit Count |
| --- | ---: |
| pending | 70 |
| ported | 4 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `d8cc85dcdccf` | #20 | review(stt-xai): address cetej's nits |
| `77f99c4ff445` | #26 | chore(release): map zhouxiaoya12 in AUTHOR_MAP |
| `85cc12e2bd55` | #26 | chore(release): map roytian1217 in AUTHOR_MAP |
| `92e4bbc201e6` | #22 | Update Docker guide with terminal command |
| `fa47cbd45671` | #26 | chore(release): map minorgod in AUTHOR_MAP |
| `156b3583206d` | #22 | docs(cron): explain runtime resolution for null model/provider |
| `48dc8ef1d158` | #22 | docs(cron): clarify default model/provider setup for scheduled jobs |
| `e8cba18f77c2` | #26 | chore(release): map wenhao7 in AUTHOR_MAP |
| `15efb410d035` | #26 | fix(nix): make working directory writable |
| `5e76c650bbae` | #26 | chore(release): map yzx9 in AUTHOR_MAP |
| `1c532278ae70` | #26 | chore(release): map lvnilesh in AUTHOR_MAP |
| `738d0900fddd` | #26 | refactor: migrate auxiliary_client Anthropic path to use transport |
| `f4612785a485` | #20 | refactor: collapse normalize_anthropic_response to return NormalizedResponse directly |
| `43de1ca8c287` | #20 | refactor: remove _nr_to_assistant_message shim + fix flush_memories guard |
| `36adcebe6ca8` | #22 | Rename API call function to _interruptible_api_call |
| `f77da7de42a1` | #22 | Rename _api_call_with_interrupt to _interruptible_api_call |
| `48923e5a3d8f` | #26 | chore(release): map azhengbot in AUTHOR_MAP |
| `d7452af257b9` | #26 | fix(pairing): handle null user_name in pairing list display |
| `b5ec6e8df79f` | #26 | chore(release): map sharziki in AUTHOR_MAP |
| `1df0c812c43a` | #26 | feat(skills): add MiniMax-AI/cli as default skill tap |
| `c80cc8557ed0` | #26 | chore(release): map RyanLee-Dev in AUTHOR_MAP |
| `a5b0c7e2ec07` | #20 | fix(config): preserve list-format models in custom_providers normalize |
| `33773ed5c6da` | #26 | chore(release): map DrStrangerUJN in AUTHOR_MAP |
| `5d0947434864` | #26 | fix(tools): enforce ACP transport overrides in delegate_task child agents |
| `911f57ad979d` | #26 | chore(release): map TaroballzChen in AUTHOR_MAP |
| `be99feff1f42` | #20 | fix(image-gen): force-refresh plugin providers in long-lived sessions |
| `8f50f2834a0d` | #26 | chore(release): add Wysie to AUTHOR_MAP |
| `9dba75bc3862` | #23 | fix(feishu): issue where streaming edits in Feishu show extra leading newlines |
| `08cb345e242e` | #26 | chore(release): map Lind3ey in AUTHOR_MAP |
| `51c1d2de16bc` | #20 | fix(profiles): stage profile imports to prevent directory clobbering |
| `4c02e4597ec9` | #26 | fix(status): catch OSError in os.kill(pid, 0) for Windows compatibility |
| `dab36d9511ce` | #26 | chore(release): map phpoh in AUTHOR_MAP |
| `7ca2f70055d9` | #22 | fix(docs): Add links to Atropos and wandb in user guide |
| `4f4fd2114949` | #26 | chore(release): map vivganes in AUTHOR_MAP |
| `78e213710ca4` | #26 | fix: guard against None tirith path in security scanner |
| `cd9cd1b159f8` | #26 | chore(release): map MikeFac in AUTHOR_MAP |
| `b24d239ce177` | #26 | Update permissions for config.yaml |
| `6172f95944d5` | #26 | chore(release): map GuyCui in AUTHOR_MAP |
| `39fcf1d12712` | #20 | fix(model_switch): group custom_providers by endpoint in /model picker (#9210) |
| `627abbb1eaf5` | #26 | chore(release): map davidvv in AUTHOR_MAP |
| `fdcb3e9a4b56` | #26 | chore(dev): add ty type checker to dev deps and configure in pyproject.toml (#14525) |
| `91d6ea07c86b` | #26 | chore(dev): add ruff linter to dev deps and configure in pyproject.toml (#14527) |
| `3a97fb3d4772` | #20 | fix(skills_sync): don't poison manifest on new-skill collision |
| `24e8a6e701ea` | #20 | feat(skills_sync): surface collision with reset-hint |
| `d50be05b1cca` | #26 | chore(release): map j0sephz in AUTHOR_MAP |
| `d45c738a52eb` | #20 | fix(gateway): preflight user D-Bus before systemctl --user start (#14531) |
| `5a26938aa502` | #20 | fix(terminal): auto-source ~/.profile and ~/.bash_profile so n/nvm PATH survives (#14534) |
| `d72985b7ce4b` | #23 | fix(gateway): serialize reset command handoff and heal stale session locks |
| `b7bdf32d4eb4` | #26 | fix(gateway): guard session slot ownership after stop/reset |
| `ec02d905c9ff` | #20 | test(gateway): regressions for issue #11016 split-brain session locks |
| `81d925f2a550` | #26 | chore(release): map dyxushuai and etcircle in AUTHOR_MAP |
| `5651a73331a8` | #23 | fix(gateway): guard-match the finally-block _active_sessions delete |
| `e3c008414075` | #20 | fix(skills-guard): allow agent-created dangerous verdicts without confirmation |
| `ce089169d578` | #20 | feat(skills-guard): gate agent-created scanner on config.skills.guard_agent_created (default off) |
| `bc9518f660c7` | #22 | fix(ui-tui): force full xterm.js alt-screen repaints |
| `071bdb5a3f09` | #22 | Revert "fix(ui-tui): force full xterm.js alt-screen repaints" |
| `82a0ed1afb3f` | #20 | feat: add Xiaomi MiMo v2.5-pro and v2.5 model support (#14635) |
| `2e7546006697` | #22 | test(ui-tui): add log-update diff contract tests |
| `f7e86577bc25` | #22 | fix(ui-tui): heal xterm.js resize-burst render drift |
| `3e01de0b092c` | #22 | fix(ui-tui): preserve composer after resize-burst healing |
| `60d1edc38a0e` | #22 | fix(ui-tui): keep bottom statusbar in composer layout |
| `e91be4d7dcc2` | #26 | fix: resolve_alias prefers highest version + merges static catalog |
| `7c4dd7d660f3` | #22 | refactor(ui-tui): collapse xterm.js resize settle dance |
| `f28f07e98eda` | #22 | test(ui-tui): drop dead terminalReally from drift repro |
| `1e445b2547c5` | #22 | fix(ui-tui): heal post-resize alt-screen drift |
| `c8ff70fe03f5` | #22 | perf(ui-tui): freeze offscreen live tail during scroll |
| `aa47812edfb9` | #22 | fix(ui-tui): clear sticky prompt when follow snaps to bottom |
| `9a885fba31e5` | #22 | fix(ui-tui): hide stale sticky prompt when newer prompt is visible |
| `9bf6e1cd6eee` | #22 | refactor(ui-tui): clean touched resize and sticky prompt paths |
| `882278520ba9` | #22 | chore: uptick |

