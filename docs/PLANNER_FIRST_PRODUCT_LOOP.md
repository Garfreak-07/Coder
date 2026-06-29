# Planner-First Product Loop

Coder's ordinary product loop starts with Planner conversation and only then
enters execution.

```text
User
-> Planner Chat in Discuss mode
-> structured PlanDraft and readiness
-> Work mode confirmation
-> WorkflowRunner
-> role-specific HarnessSpec
-> planner-model, native Rust, or OpenHands backend
-> events, approvals, evidence, patches, checks
-> evidence-backed final report
-> Planner-facing result summary
```

## Discuss Mode

Discuss mode is conversational and side-effect free.

It can:

- answer casual questions
- ask clarifying questions
- discuss scope, risks, assumptions, and acceptance criteria
- maintain a small plan draft
- propose user/project memory updates for review
- mark readiness

It must not:

- write files
- run commands
- start a workflow
- silently write global memory
- claim execution happened

Backend contract:

- `PlannerConversationEngine::respond`
- `PlannerConversationRequest`
- `PlannerConversationResponse`
- `PlanDraft`

Planner Chat resolves runtime from the workflow:

```text
workflow_id -> planner node -> AgentSpec -> HarnessSpec -> model profile
```

The planner node must bind to the read-only `planner-model` HarnessSpec. The
deterministic engine is only used in mock/test mode. In product mode, missing
model provider configuration returns:

```text
Planner model provider is not configured.
Set LLM_BASE_URL and LLM_API_KEY or configure provider settings.
```

PlanDraft includes `memory_proposals`. These are not writes. Durable project
memory requires an explicit `memory.write.proposed` event followed by a
`memory.write.confirmed` request with `confirmed_by_role = planning_chat`.
Workflow supervisor and task execution roles cannot propose or confirm project
memory writes.

## Work Mode

Work mode validates the current or newly generated plan. It blocks when open
questions remain. It starts execution only when readiness is `ready` and the
turn is explicitly confirmed.

The Work-mode run request carries:

- `original_user_request`
- `planner_conversation_summary`
- `plan_draft`
- `acceptance_criteria`
- `risks`
- `affected_paths`
- `selected_workflow_id`

`WorkflowRunner` records this plan context in run events, passes it into
harness backend context, projects it into OpenHands payloads, and includes plan
summary and acceptance criteria in the final report checks.

## Important Files

- Backend Planner Chat: `crates/coder-server/src/lib.rs`
- Workflow execution: `crates/coder-workflow/src/lib.rs`
- Harness boundary: `crates/coder-config/src/lib.rs`,
  `crates/coder-harness/src/lib.rs`
- OpenHands client: `crates/coder-openhands/src/lib.rs`
- React Planner UI: `frontend/src/features/planner-chat/PlannerChatPage.tsx`
- React API adapter: `frontend/src/api.ts`
