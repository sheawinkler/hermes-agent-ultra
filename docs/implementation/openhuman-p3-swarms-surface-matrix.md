# OpenHuman P3 Swarms Surface Matrix

| Surface | Command/API | Primary code paths | Tests |
|---|---|---|---|
| Swarm runtime status | `/swarm status` | `hermes-intelligence/src/swarm_runtime.rs`, `handle_swarm_command` | `p3_swarm_status_plan_run_cancel_surface_is_handled` |
| Deterministic swarm plan | `/swarm plan [mode]` | `build_swarm_execution_plan`, `parse_swarm_mode`, `read_swarm_pass_cap` | `p3_swarm_status_plan_run_cancel_surface_is_handled`, `required_success_uses_majority_when_voters_gt_two` |
| One-shot swarm execution arm | `/swarm run [passes] [mode]` | `handle_swarm_command` + quorum arming (`app.quorum_armed_once`) | `p3_swarm_status_plan_run_cancel_surface_is_handled` |
| Cancel/disarm path | `/swarm cancel` | `handle_swarm_command`, `clear_quorum_system_hints` | `p3_swarm_status_plan_run_cancel_surface_is_handled` |
| Artifact introspection | `/swarm artifact` | `latest_quorum_artifact_path`, artifact summary readback | command handler coverage in `--lib` suite |
| Compatibility aliasing | `/swarms` -> `/swarm` | `canonical_command` mapping | `test_upstream_compat_aliases_are_mapped`, `p3_swarm_commands_registered_and_completable` |

