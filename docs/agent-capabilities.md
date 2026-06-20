# Agent Capability Boundaries

Coder uses explicit authority profiles so prompts are not the only enforcement
point for Planner, Worker, Tester, and Final Tester behavior.

## Authority Profiles

Planner authority:

- Can ask the user.
- Can create `planner_order`, `planner_decision`, `round_summary`, and `run_contract`.
- Can write workflow memory.
- Owns global `continue`, `ask_human`, `finish`, and `stop` decisions.

Worker authority:

- Can create `execution_result`.
- Can report Planner intervention blockers.
- Can propose local file changes when capability policy allows it.
- Cannot ask the user or decide global continuation.

Tester authority:

- Can create `test_result`.
- Can propose optional command evidence when capability policy allows it.
- Cannot ask the user or decide global continuation.

Final Tester authority:

- Can create aggregate `test_result` artifacts.
- Compresses evidence for Planner.
- Cannot ask the user or decide global continuation.

## Skill Modules

Planner modules:

- `task_modeling`
- `plan_graph_decomposition`
- `capability_routing`
- `dependency_planning`
- `risk_judgment`
- `replanning`
- `human_escalation`
- `memory_read_write`
- `tool_policy_planning`

Worker modules:

- `follow_task_envelope`
- `local_execution`
- `proposed_changes`
- `blocker_reporting`
- `execution_result_output`

Tester modules:

- `evidence_review`
- `check_command_proposal`
- `test_result_output`
- `confidence_calibration`

Final Tester modules:

- `aggregate_tests`
- `round_evidence_compression`

These modules are a registry boundary first. Runtime strategy can attach richer
logic to each module later without changing the user-facing AgentWorkflow shape.
