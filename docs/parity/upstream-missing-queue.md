# Upstream Missing Patch Queue

Generated: `2026-04-23T06:43:51.538395+00:00`

- Range: `main..upstream/main`; total commits tracked: `4766`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 1672 |
| #21 | GPAR-02 skills parity | 202 |
| #22 | GPAR-03 UX parity | 557 |
| #23 | GPAR-04 gateway/plugin-memory parity | 520 |
| #24 | GPAR-05 environments+parsers+benchmarks | 66 |
| #25 | GPAR-06 packaging/docs/install parity | 156 |
| #26 | GPAR-07 upstream queue backfill | 1593 |

| Disposition | Commit Count |
| --- | ---: |
| pending | 144 |
| ported | 92 |
| superseded | 4530 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `d166716c65ea` | #21 | feat(optional-skills): add page-agent skill under new web-development category (#13976) |
| `8bcd77a9c2e8` | #23 | feat(wecom): add QR scan flow and interactive setup wizard for bot credentials |
| `3f60a907e1d3` | #22 | docs(wecom): document QR scan-to-create setup flow |
| `b43524ecabc3` | #23 | fix(wecom): visible poll progress + clearer no-bot-info failure + docstring note |
| `b66644f0ecce` | #23 | feat(hindsight): richer session-scoped retain metadata |
| `cf55c738e79b` | #23 | refactor(qqbot): migrate qr onboard flow to sync + consolidate into onboard.py |
| `5fb143169b4e` | #20 | feat(dashboard): track real API call count per session |
| `7785654ad5cc` | #22 | feat(tui): subagent spawn observability overlay |
| `06ebe34b4005` | #22 | fix(tui): repair useInput handler in agents overlay |
| `f06adcc1ae0c` | #22 | chore(tui): drop unreachable return + prettier pass |
| `70a33708e7c9` | #23 | fix(gateway/slack): align reaction lifecycle with Discord/Telegram pattern |
| `1f216ecbb479` | #23 | feat(gateway/slack): add SLACK_REACTIONS env toggle for reaction lifecycle |
| `dee51c160764` | #22 | fix(tui): address Copilot review on #14045 |
| `82197a87dcac` | #22 | style(tui): breathing room around status glyphs in agents overlay |
| `eda400d8a58c` | #22 | chore: uptick |
| `7eae504d158b` | #22 | fix(tui): address Copilot round-2 on #14045 |
| `9e1f606f7f92` | #22 | fix: scroll in agents detail view |
| `5b0741e986c9` | #22 | refactor(tui): consolidate agents overlay — share duration/root helpers via lib |
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
| `e0d698cfb351` | #22 | fix(tui): yolo toggle only reports on/off for strict '0'/'1' values |
| `8410ac05a9cc` | #22 | fix(tui): tab title shows cwd + waiting-for-input marker |
| `103c71ac36c6` | #22 | refactor(tui): /clean pass on tui-polish — data tables, tighter title |
| `4107538da830` | #22 | style(debug): add missing blank line between LogSnapshot and helpers |
| `ea9ddecc72d1` | #22 | fix(tui): route Ctrl+K and Ctrl+W through macOS readline fallback |
| `e86acad8f1a5` | #23 | feat(feishu): preserve @mention context on inbound messages |
| `44a16c5d9d54` | #20 | guard terminal_tool import-time env parsing |
| `83efea661f83` | #22 | fix(tui): address copilot round 3 on #14145 |
| `c96a548bde1b` | #26 | feat(models): add xiaomi/mimo-v2.5-pro and mimo-v2.5 to openrouter + nous (#14184) |
| `51ca57599466` | #20 | feat(gateway): expose plugin slash commands natively on all platforms + decision-capable command hook |
| `66d2d7090e76` | #26 | fix(model_metadata): add gemma-4 and gemma4 context length entries |
| `3c54ceb3cafe` | #26 | chore(release): add AUTHOR_MAP entry for Feranmi10 |
| `284e084bcc06` | #26 | perf(browser): upgrade agent-browser 0.13 -> 0.26, wire daemon idle timeout |
| `b52123eb158b` | #20 | fix(gateway): recover stale pid and planned restart state |
| `402d048eb6c6` | #20 | fix(gateway): also unlink stale PID + lock files on cleanup |
| `10063e730c9b` | #26 | [verified] docs: fix broken env var example in contributing guide |
| `dad53205ea4d` | #26 | chore(release): map simon-gtcl in AUTHOR_MAP |
| `e67eb7ff4b79` | #26 | fix(gateway): add hermes-gateway script pattern to PID detection |
| `12f9f10f0f6a` | #26 | chore(release): map houko in AUTHOR_MAP |
| `27621ef83690` | #26 | feat: add ctx_size to context length keys for Lemonade server support |
| `e710bb1f7f99` | #26 | chore(release): map cgarwood82 in AUTHOR_MAP |
| `e826cc42ef07` | #26 | fix(nix): use stdenv.hostPlatform.system instead of system |
| `80108104cf92` | #26 | chore(release): map anna-oake in AUTHOR_MAP |
| `c47d4eda13be` | #26 | fix(tools): restrict RPC socket permissions to owner-only |
| `ea0e4c267d87` | #26 | chore(release): map jaffarkeikei in AUTHOR_MAP |
| `9eb543cafe4d` | #20 | feat(/model): merge models.dev entries for lesser-loved providers (#14221) |
| `c0df4a0a7f0b` | #23 | fix(email): accept **kwargs in send_document to handle metadata param |
| `0187de1f67cf` | #26 | chore(release): map hxp-plus in AUTHOR_MAP |
| `953f8fa943e3` | #26 | fix(scripts): read gateway_voice_mode.json as UTF-8 |
| `0dace06db7c3` | #26 | chore(release): map Tianworld in AUTHOR_MAP |
| `276ef49c9610` | #26 | fix(provider): recognize open.bigmodel.cn as Zhipu/ZAI provider |
| `ea83cd91e407` | #26 | chore(release): map wujhsu in AUTHOR_MAP |
| `3445530dbf18` | #26 | feat(web): support TAVILY_BASE_URL env var for custom proxy endpoints |
| `3e95963bde2a` | #26 | chore(release): map niyoh120 in AUTHOR_MAP |
| `435d86ce36b6` | #24 | fix: use builtin cd in command wrapper to bypass shell aliases |
| `75221db96796` | #26 | chore(release): map vrinek in AUTHOR_MAP |
| `02aba4a728e2` | #26 | fix(skills): follow symlinks in iter_skill_index_files |
| `6f629a04622d` | #26 | chore(release): map xandersbell in AUTHOR_MAP |
| `5fbb69989da0` | #25 | fix(docker): add openssh-client for SSH terminal backend |
| `c0100dde3553` | #26 | chore(release): map Somme4096 in AUTHOR_MAP |
| `4009f2edd9bd` | #25 | feat(docker): add docker-cli to Docker image |
| `b2593c8d4ec3` | #26 | chore(release): map brianclemens in AUTHOR_MAP |
| `96b0f3700117` | #26 | fix: separate browser_cdp into its own toolset |
| `98e1396b1569` | #26 | chore(release): map yudaiyan in AUTHOR_MAP |
| `3e96c87f371e` | #20 | fix(delegate): make MCP toolset inheritance configurable |
| `7d8b2eee638f` | #20 | fix(delegate): default inherit_mcp_toolsets=true, drop version bump |
| `db86ed199082` | #26 | fix(terminal): forward docker_forward_env and docker_env to container_config The container_config builder in terminal_tool.py was missing docker_forward_env and docker_env keys, causing config.yaml's docker_forward_env setting to be silently ignored. Environment variables listed in docker_forward_env were never injected into Docker containers. This fix adds both keys to the container_config dict so they are properly passed to _create_environment(). |
| `142202910e96` | #26 | chore(release): map ycbai in AUTHOR_MAP |
| `846b9758d879` | #25 | Remove Discussions link from README |
| `54db93366781` | #26 | chore(release): map longsizhuo in AUTHOR_MAP |
| `8db5517b4cc7` | #25 | fix: add /opt/data/.local/bin to PATH in Docker image (Closes #13739) |
| `9ea2d96d7355` | #26 | chore(release): map ms-alan in AUTHOR_MAP |
| `4c1362884dcb` | #24 | fix(local): respect configured cwd in init_session() |
| `e5114298f00d` | #26 | chore(release): map WuTianyi123 in AUTHOR_MAP |
| `82cce3d26ca1` | #26 | fix: add base_url_env_var to Anthropic ProviderConfig |
| `08089738d888` | #26 | chore(release): map li0near in AUTHOR_MAP |
| `c03858733d7f` | #26 | fix: pass correct arguments in summary model fallback retry |
| `8152de2a844f` | #26 | chore(release): map sicnuyudidi in AUTHOR_MAP |
| `9bd15184256f` | #23 | fix(feishu): correct identity model docs and prefer tenant-scoped user_id |
| `c345ec9a6384` | #20 | fix(display): strip standalone tool-call XML tags from visible text |
| `a3014a4481c8` | #20 | fix(docker): add SETUID/SETGID caps so gosu drop in entrypoint succeeds |
| `d70f0f1dc03f` | #26 | fix(docker): allow entrypoint to pass-through non-hermes commands |
| `159061836e1a` | #26 | chore(release): map @akhater's Azure VM commit email in AUTHOR_MAP |
| `aa75d0a90b1b` | #22 | fix(web): remove duplicate skill count in dashboard badge (#12372) |
| `50387d718e63` | #26 | chore(release): map haimu0x in AUTHOR_MAP |
| `ce4214ec94d8` | #26 | Normalize claw workspace paths for Windows |
| `86510477f330` | #26 | chore(release): map NIDNASSER-Abdelmajid in AUTHOR_MAP |
| `d67d12b5df3d` | #26 | Update whatsapp-bridge package-lock.json |
| `2c26a8084854` | #26 | chore(release): map projectadmin-dev in AUTHOR_MAP |
| `a14fb3ab1ac4` | #26 | fix(cli): guard fallback_model list format in save_config_value |
| `6ad2fab8cfaa` | #26 | chore(release): map Dev-Mriganka in AUTHOR_MAP |

