# Upstream Missing Patch Queue

Generated: `2026-04-22T22:23:25.620880+00:00`

- Range: `main..upstream/main`; total commits tracked: `4654`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 1654 |
| #21 | GPAR-02 skills parity | 201 |
| #22 | GPAR-03 UX parity | 551 |
| #23 | GPAR-04 gateway/plugin-memory parity | 514 |
| #24 | GPAR-05 environments+parsers+benchmarks | 64 |
| #25 | GPAR-06 packaging/docs/install parity | 152 |
| #26 | GPAR-07 upstream queue backfill | 1518 |

| Disposition | Commit Count |
| --- | ---: |
| pending | 65 |
| ported | 68 |
| superseded | 4521 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `c6b1ef4e5881` | #20 | feat: add Step Plan provider support (salvage #6005) |
| `30ec12970b10` | #26 | fix(packaging): include agent.* sub-packages in pyproject.toml |
| `a7d78d3bfd81` | #20 | fix: preserve reasoning_content on Kimi replay |
| `d166716c65ea` | #21 | feat(optional-skills): add page-agent skill under new web-development category (#13976) |
| `8bcd77a9c2e8` | #23 | feat(wecom): add QR scan flow and interactive setup wizard for bot credentials |
| `3f60a907e1d3` | #22 | docs(wecom): document QR scan-to-create setup flow |
| `b43524ecabc3` | #23 | fix(wecom): visible poll progress + clearer no-bot-info failure + docstring note |
| `b8663813b667` | #20 | feat(state): auto-prune old sessions + VACUUM state.db at startup (#13861) |
| `b66644f0ecce` | #23 | feat(hindsight): richer session-scoped retain metadata |
| `ba7e8b0df9ee` | #26 | chore(release): map Abner email to Abnertheforeman |
| `cf55c738e79b` | #23 | refactor(qqbot): migrate qr onboard flow to sync + consolidate into onboard.py |
| `be11a75eaec4` | #26 | chore(release): map hharry11 email to GitHub handle |
| `5fb143169b4e` | #20 | feat(dashboard): track real API call count per session |
| `3e652f75b27b` | #20 | fix(plugins+nous): auto-coerce memory plugins; actionable Nous 401 diagnostic (#14005) |
| `77e04a29d574` | #20 | fix(error_classifier): don't classify generic 404 as model_not_found (#14013) |
| `2efb0eea211a` | #26 | fix(anthropic_adapter): preserve reasoning_content on assistant tool-call messages for Kimi /coding |
| `97a536057ddf` | #26 | chore(release): add hiddenpuppy to AUTHOR_MAP |
| `04e039f687b8` | #26 | fix: Kimi /coding thinking block survival + empty reasoning_content + block ordering |
| `7785654ad5cc` | #22 | feat(tui): subagent spawn observability overlay |
| `06ebe34b4005` | #22 | fix(tui): repair useInput handler in agents overlay |
| `f06adcc1ae0c` | #22 | chore(tui): drop unreachable return + prettier pass |
| `70a33708e7c9` | #23 | fix(gateway/slack): align reaction lifecycle with Discord/Telegram pattern |
| `1f216ecbb479` | #23 | feat(gateway/slack): add SLACK_REACTIONS env toggle for reaction lifecycle |
| `5e8262da26a6` | #26 | chore: add rnijhara to AUTHOR_MAP |
| `dee51c160764` | #22 | fix(tui): address Copilot review on #14045 |
| `82197a87dcac` | #22 | style(tui): breathing room around status glyphs in agents overlay |
| `eda400d8a58c` | #22 | chore: uptick |
| `7eae504d158b` | #22 | fix(tui): address Copilot round-2 on #14045 |
| `9e1f606f7f92` | #22 | fix: scroll in agents detail view |
| `5b0741e986c9` | #22 | refactor(tui): consolidate agents overlay — share duration/root helpers via lib |
| `fc3862bdd637` | #20 | fix(debug): snapshot logs once for debug share |
| `921133cfa56a` | #26 | fix(debug): preserve full line at truncation boundary and cap memory |
| `61d0a99c11cd` | #20 | fix(debug): sweep expired pending pastes on slash debug paths |
| `8dc936f10ecf` | #20 | chore: add taosiyuan163 to AUTHOR_MAP, add truncation boundary tests |
| `de849c410da9` | #20 | refactor(debug): remove dead _read_log_tail/_read_full_log wrappers |
| `c32321718822` | #20 | fix: make CLI status bar skin-aware |
| `81a504a4a0f3` | #20 | fix: align status bar skin tests with upstream main |
| `88564ad8bc75` | #26 | fix(skins): don't inherit status_bar_* into light-mode skins |
| `7027ce42efd2` | #22 | fix(tui): blitz closeout — input wrap parity, shift-tab yolo, bottom statusline |
| `d55a17bd824c` | #22 | refactor(tui): statusbar as 4-mode position (on\|off\|bottom\|top) |
| `ea32364c9655` | #22 | fix(tui): /statusbar top = inline above input, not row 0 of the screen |
| `408fc893e93c` | #22 | fix(tui): tighten composer — status sits directly above input, overlays anchor to input |
| `a7cc903bf58b` | #22 | fix(tui): breathing room above the composer cluster, status tight to input |
| `88993a468f30` | #22 | fix(tui): input wrap width mismatch — last letter no longer flickers |
| `1e8cfa909219` | #22 | fix(tui): idle good-vibes heart no longer blanks the input's last cell |
| `48f2ac33528e` | #22 | refactor(tui): /clean pass on blitz closeout — trim comments, flatten logic |
| `6fb98f343a7b` | #22 | fix(tui): address copilot review on #14103 |
| `3ef6992edf2b` | #22 | fix(tui): drop main-screen banner flash, widen alt-screen clear on entry |
| `b641639e425b` | #20 | fix(debug): distinguish empty-log from missing-log in report placeholder |
| `ea67e49574b0` | #20 | fix(streaming): silent retry when stream dies mid tool-call (#14151) |
| `e0d698cfb351` | #22 | fix(tui): yolo toggle only reports on/off for strict '0'/'1' values |
| `b49a1b71a738` | #20 | fix(agent): accept empty content with stop_reason=end_turn as valid anthropic response |
| `8410ac05a9cc` | #22 | fix(tui): tab title shows cwd + waiting-for-input marker |
| `103c71ac36c6` | #22 | refactor(tui): /clean pass on tui-polish — data tables, tighter title |
| `4107538da830` | #22 | style(debug): add missing blank line between LogSnapshot and helpers |
| `ea9ddecc72d1` | #22 | fix(tui): route Ctrl+K and Ctrl+W through macOS readline fallback |
| `d6ed35d04764` | #20 | feat(security): add global toggle to allow private/internal URL resolution |
| `76c454914a7c` | #20 | fix(core): ensure non-blocking executor shutdown on async timeout |
| `4ac1c959b250` | #20 | fix(agent): resolve fallback provider key_env secrets |
| `e86acad8f1a5` | #23 | feat(feishu): preserve @mention context on inbound messages |
| `44a16c5d9d54` | #20 | guard terminal_tool import-time env parsing |
| `6513138f2684` | #20 | fix(agent): recognize Tailscale CGNAT (100.64.0.0/10) as local for Ollama timeouts |
| `2e5ddf9d2e8c` | #26 | chore(release): add AUTHOR_MAP entry for ismell0992-afk |
| `1e8254e59962` | #20 | fix(agent): guard context compressor against structured message content |
| `83efea661f83` | #22 | fix(tui): address copilot round 3 on #14145 |

