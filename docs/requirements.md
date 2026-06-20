# Coder Requirements

## Document Status

This is the canonical product requirements document for Coder. It replaces the
older Trust Runtime and generic workflow-builder requirement track.

The new direction is:

```text
Planner-led Orchestrator
+ Structured Artifact Handoff
+ Agent-only Workflow UI
+ Hidden Runtime Graph
```

Older implementation assets can remain when they support this direction, but
the public product should no longer be described as a generic node-and-tool
workflow editor. The near-term product is a local-first Planner-led agent
workflow workbench for controlled coding tasks.

## Product Goal

Coder helps users run local AI coding workflows where a strong Planner keeps
global control, weaker worker agents execute and test, and every handoff is a
small structured artifact instead of a full transcript.

Coder is not:

- a generic low-code automation clone;
- a free-form multi-agent chat room;
- a direct wrapper around LangGraph, CrewAI, LlamaIndex, AutoGen, or the
  OpenAI Agents SDK;
- a marketplace-first product;
- a GitHub automation bot as the primary surface.

Coder should absorb useful ideas from those systems, especially orchestrator
patterns, custom planners, structured output, handoff filtering, and graph
runtime concepts. It should not expose their internal product shape to ordinary
users.

## Core Product Thesis

The first useful version is a default three-agent loop:

```text
Planner Agent -> Executor Agent -> Tester Agent
      ^                                   |
      +----------- loop decision ---------+
```

The runtime expands this into internal nodes only after the user-visible
workflow has been saved or run.

```text
User layer:
  Planner / Executor / Tester agents and handoff edges

Internal runtime layer:
  run contract, planner order, execution, test, planner decision,
  round summary, loop routing, storage, context selection, hard guards
```

The user should understand the product by reading the agent cards, not by
learning tool nodes, condition nodes, MCP nodes, hidden approval nodes, or graph
engine internals.

## Roles and Authority

| Role | Can Do | Cannot Do |
| --- | --- | --- |
| Planner | Understand the goal, negotiate the RunContract, create orders, judge risk, decide continue/finish/ask_human/stop | Directly mutate files |
| Executor | Follow PlannerOrder, perform authorized implementation work, return ExecutionResult facts | Ask the human, redefine the goal, decide completion |
| Tester | Review execution evidence, optionally use check results, return TestResult evidence | Ask the human, decide next round, mutate files |
| Runtime | Enforce hard limits, execute graph, store artifacts, select context, enforce path/tool/token boundaries | Make subjective risk decisions |
| Human | Talk to Planner | Talk directly to Executor or Tester |

Only Planner can ask the human. Only Planner makes subjective risk and next-step
decisions. Runtime guardrails still enforce hard boundaries such as path scope,
max rounds, token budgets, command approval, and schema validation.

## Product Principles

1. User-visible workflows are agent-only by default.
   Runtime graph details are an implementation detail.

2. Planner owns global decisions.
   Executor and Tester return facts and evidence.

3. Agents exchange artifacts, not transcripts.
   Full conversation history, full event logs, full diffs, and full tool output
   are never passed by default.

4. Structured output is a product contract.
   The artifact schemas are the core product interface between agents.

5. Context is filtered per role.
   Executor sees PlannerOrder and required references. Tester sees
   ExecutionResult and relevant evidence. Planner sees summaries plus the latest
   structured results.

6. The runtime remains local-first and conservative.
   File mutation, command execution, network access, and external tools remain
   scoped, auditable, and approval-aware.

7. External frameworks are adapters or references.
   They must not define Coder's saved workflow format.

## P0 Requirements

P0 is the smallest landing plan that proves the new direction quickly.

### P0.1 Structured Artifact Protocol

Coder must support these six artifacts as first-class validated runtime
objects:

- `run_contract`
- `planner_order`
- `execution_result`
- `test_result`
- `planner_decision`
- `round_summary`

These artifacts replace the old default coding workflow's primary
`plan_artifact`, `patch_artifact`, and `review_artifact` contract. The legacy
types may remain for compatibility with old saved workflows, but new default
workflows should use the six-artifact protocol.

### P0.2 RunContract

RunContract is the global agreement between Planner and the human-facing user.
It defines the run goal, done criteria, scope, loop policy, risk policy,
execution policy, and test policy.

Required shape:

```json
{
  "artifact_type": "run_contract",
  "user_goal": "",
  "done_criteria": [],
  "scope": {
    "allowed_paths": [],
    "forbidden_paths": []
  },
  "loop_policy": {
    "max_auto_rounds": 3,
    "user_can_override": true
  },
  "risk_policy": {
    "planner_is_risk_judge": true,
    "high_risk_requires_human": true,
    "low_risk_auto_continue": true
  },
  "execution_policy": {
    "executor_can_modify_files": true,
    "executor_cannot_ask_human": true,
    "executor_must_follow_planner_order": true
  },
  "test_policy": {
    "default_mode": "model_review_and_optional_command",
    "tester_cannot_ask_human": true
  },
  "human_agreements": []
}
```

### P0.3 PlannerOrder

PlannerOrder is the only instruction object Executor should follow for a round.

```json
{
  "artifact_type": "planner_order",
  "round": 1,
  "round_goal": "",
  "instructions_for_executor": [],
  "allowed_actions": [],
  "forbidden_actions": [],
  "target_files_or_outputs": [],
  "expected_outputs": [],
  "risk_level": "low",
  "requires_human_confirmation": false,
  "tester_instructions": [],
  "stop_and_return_to_planner_when": []
}
```

`stop_and_return_to_planner_when` is important. It lets Executor stop and return
to Planner when the order is insufficient without asking the human directly.

### P0.4 ExecutionResult

ExecutionResult is Executor's factual report.

```json
{
  "artifact_type": "execution_result",
  "round": 1,
  "status": "completed",
  "summary": "",
  "changed_files": [],
  "created_files": [],
  "deleted_files": [],
  "patch_refs": [],
  "outputs": [],
  "unexpected_issues": [],
  "out_of_contract": false,
  "needs_planner_decision": false,
  "tester_notes": []
}
```

Executor does not decide whether the task is complete.

### P0.5 TestResult

TestResult is Tester's evidence report.

```json
{
  "artifact_type": "test_result",
  "round": 1,
  "status": "pass",
  "summary": "",
  "evidence": [],
  "issues": [],
  "remaining_work": [],
  "confidence": "medium",
  "check_commands": [],
  "check_outputs_ref": null
}
```

Tester does not decide whether to continue or finish.

### P0.6 PlannerDecision

PlannerDecision closes the round.

```json
{
  "artifact_type": "planner_decision",
  "round": 1,
  "task_done": false,
  "next_action": "continue",
  "risk_level": "low",
  "requires_human_confirmation": false,
  "reason": "",
  "next_round_goal": "",
  "remaining_auto_rounds": 2,
  "human_message": null
}
```

Allowed `next_action` values:

```text
continue
ask_human
finish
stop
```

### P0.7 RoundSummary

RoundSummary is the compressed carry-forward record. Planner should use
RoundSummary plus the latest ExecutionResult and TestResult instead of reading
the full historical event log.

```json
{
  "artifact_type": "round_summary",
  "round": 1,
  "planner_order_summary": "",
  "execution_summary": "",
  "test_summary": "",
  "planner_decision_summary": "",
  "important_refs": [],
  "carry_forward_constraints": [],
  "remaining_work": []
}
```

## AgentWorkflowSpec

Coder needs a smaller user-visible workflow schema above the current runtime
schema.

```json
{
  "id": "default-planner-led",
  "name": "Planner-led Agent Workflow",
  "agents": [
    {
      "id": "planner",
      "name": "Planner Agent",
      "model_tier": "best",
      "can_talk_to_human": true,
      "capabilities": [
        "negotiate_contract",
        "make_plan",
        "judge_completion",
        "judge_risk",
        "make_next_decision"
      ]
    },
    {
      "id": "executor",
      "name": "Executor Agent",
      "model_tier": "standard",
      "can_talk_to_human": false,
      "capabilities": [
        "modify_files",
        "follow_planner_order",
        "return_execution_result"
      ]
    },
    {
      "id": "tester",
      "name": "Tester Agent",
      "model_tier": "standard",
      "can_talk_to_human": false,
      "capabilities": [
        "model_review",
        "optional_check_command",
        "return_test_result"
      ]
    }
  ],
  "edges": [
    {
      "from": "planner",
      "to": "executor",
      "handoff": "planner_order"
    },
    {
      "from": "executor",
      "to": "tester",
      "handoff": "execution_result"
    },
    {
      "from": "tester",
      "to": "planner",
      "handoff": "test_result",
      "loop": true
    }
  ],
  "loop_policy": {
    "max_auto_rounds": 3,
    "user_can_change": true
  }
}
```

`AgentWorkflowSpec` compiles into the internal `WorkflowSpec`. The compiler may
create hidden runtime nodes such as contract creation, planner order,
execution, test, decision, summary, and loop routing. Those nodes are not the
primary product surface.

## Context Handoff Requirements

Default context rules:

1. Executor receives RunContract, PlannerOrder, and required references.
2. Tester receives PlannerOrder, ExecutionResult, check output references, and
   required evidence.
3. Planner receives RunContract, RoundSummary list, latest ExecutionResult,
   latest TestResult, and necessary references.
4. Full event history is opt-in.
5. Full diffs, full logs, full blobs, and full transcripts are referenced by ID
   and loaded on demand.
6. Empty `input_keys` never means "send all state".
7. The runtime emits inspectable context packets before agent calls.

## UI Requirements

The default first screen should present the actual workflow, not a marketing
page.

Default visible canvas:

```text
[Planner Agent] -> [Executor Agent] -> [Tester Agent]
       ^                                  |
       +-------- unfinished loops --------+
```

Right-side settings should use ordinary language:

- max automatic rounds: default 3;
- high risk: ask me through Planner;
- low risk: Planner may continue automatically;
- Executor: can perform authorized implementation work;
- Tester: model review plus optional command evidence;
- only Planner can ask the user.

Advanced users may still open runtime JSON, but runtime nodes should not be the
ordinary first impression.

## Runtime and Safety Requirements

The current runtime remains useful if it serves the Planner-led product.

Keep:

- FastAPI runtime API;
- Pydantic schema validation;
- compact context packets;
- artifact validation and storage;
- event log and run replay;
- path guards and scoped file access;
- patch preview, snapshot, apply, and rollback primitives;
- command approval and audit records;
- provider settings and mock mode;
- workflow preflight checks;
- local-first file-backed run storage.

De-emphasize or hide from the ordinary product surface:

- raw tool nodes;
- MCP nodes;
- condition nodes;
- human gate nodes;
- start/end implementation nodes;
- generic marketplace concepts;
- GitHub write automation;
- browser automation;
- knowledge-base expansion beyond small local documents.

These can remain as internal capabilities or later advanced features when they
directly support the Planner-led loop.

## Current Implemented State

Implemented for the new direction:

- six new validated artifact types:
  `run_contract`, `planner_order`, `execution_result`, `test_result`,
  `planner_decision`, `round_summary`;
- `AgentWorkflowSpec` for the user-visible agent-only layer;
- compiler from default `AgentWorkflowSpec` to internal `WorkflowSpec`;
- mock executor output for all six artifacts;
- default frontend and example workflow using the Planner-led loop;
- artifact event preview support for the new protocol;
- tests proving artifact validation and default mock-mode loop execution.

Still implemented from the previous foundation and retained because it supports
the new direction:

- local FastAPI backend and React/Vite frontend;
- workflow runtime, event stream, run storage, run history, and replay;
- provider settings and mock mode;
- context packet events;
- artifact storage with blob offloading;
- path guards, patch safety primitives, command approvals, and preflight.

Legacy compatibility:

- `plan_artifact`, `patch_artifact`, and `review_artifact` remain supported for
  older saved workflows and tests, but they are not the active default product
  contract.

## Active Roadmap

### v0.3 Planner-led Default Workflow

Goal: run the default Planner -> Executor -> Tester -> Planner loop in mock
mode and validate every artifact.

Delivered:

- six-artifact protocol;
- default AgentWorkflowSpec;
- compiler into hidden runtime WorkflowSpec;
- default mock-mode loop;
- frontend and example workflow update.

Remaining:

- make PlannerDecision routing block on `ask_human` with a Planner-owned human
  prompt instead of finishing;
- show the user-visible AgentWorkflowSpec separately from the compiled runtime
  graph;
- tighten copy for Planner/Executor/Tester cards.

### v0.4 AgentWorkflowSpec Productization

Goal: users save Agent-only workflows and the runtime compiles them internally.

Deliver:

- persisted AgentWorkflowSpec library;
- compile endpoint;
- validator with ordinary-language errors;
- default three-agent loop only;
- no arbitrary custom loops yet.

### v0.5 Agent-only Canvas

Goal: the app opens to an agent-only workflow.

Deliver:

- three Agent cards;
- handoff edges;
- loop edge;
- Agent settings panel;
- max rounds and risk policy settings;
- toggle to inspect compiled runtime graph for advanced debugging.

### v0.6 Real Coding Loop

Goal: Executor can perform real controlled code changes and Tester can use
model review plus optional command checks.

Deliver:

- PlannerOrder to scoped patch workflow;
- patch preview/apply/rollback hidden behind Executor capability;
- TestResult from model review and optional check command;
- Planner high-risk ask_human decision;
- low-risk automatic continuation.

### v0.7 Custom Workflow Builder

Goal: users can build custom agent graphs without seeing raw runtime nodes.

Deliver:

- arbitrary agent nodes;
- arbitrary handoff edges;
- loop edges with max rounds;
- workflow save-time validator;
- ordinary-language topology errors;
- policy that only Planner-like agents can ask the human.

### Later Work

Do not prioritize until the Planner-led loop is reliable:

- MCP marketplace;
- skills marketplace;
- GitHub write automation;
- browser automation;
- large knowledge base management;
- PDF/Word ingestion;
- cloud sync;
- multi-user permissions;
- desktop packaging;
- arbitrary parallel/subworkflow runtime editing.

## Deletion and Simplification Policy

Delete or hide features that do not support the Planner-led product direction.

Keep a feature only if it satisfies at least one condition:

1. It is required for the default Planner-led loop.
2. It protects local files, secrets, commands, or user data.
3. It supports structured context, artifact storage, replay, or recovery.
4. It is needed for old saved workflows to keep loading while the product
   migrates.

Everything else is backlog, not active scope.
