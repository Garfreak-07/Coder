# Planner Memory OpenHands Audit

This is the implementation audit required before the Planner memory and
OpenHands-first release hardening phase.

## Findings

1. `PlannerConversationEngine` can call a live provider, but the current product
   path silently falls back to deterministic heuristics when the provider is not
   configured or fails. That must become an explicit product-mode error while
   keeping deterministic fallback for mock/test mode.
2. Planner Chat does not currently resolve its runtime from
   `workflow_id -> planner node -> AgentSpec -> HarnessSpec`. Frontend requests
   send workflow context, but the Rust request DTO ignores it.
3. Planner Chat has structured memory fields in `AgentSpec` and `HarnessSpec`,
   but the endpoint does not enforce planner-conversation memory scopes from the
   resolved harness.
4. Planner can produce plan state, but `PlanDraft` has no memory proposal field.
   Existing memory APIs support `memory.write.proposed` and
   `memory.write.confirmed`.
5. Work mode already requires readiness and confirmation before returning
   `should_start_workflow`, and frontend passes `plan_context` into
   `WorkflowRunOptions`, but the backend Planner Chat endpoint does not yet
   validate the workflow-derived Planner harness.
6. OpenHands remains the preferred executor backend when configured. The
   OpenHands payload already projects workflow, node, agent, harness, tools,
   permissions, memory scopes, plan context, and model references without secret
   values.
7. Native Rust tools are bounded fallback/preflight/evidence tools, but the
   planner node still uses a generic native read-only harness in the example
   workflow instead of an explicit Planner Conversation Harness.
8. React displays chat, plan/readiness, open questions, acceptance criteria,
   risks, run evidence, and final report surfaces. It does not yet display
   memory proposals.
9. No full duplicate OpenHands execution loop is present. The main redundant
   product behavior is the Planner product-mode heuristic fallback and old
   review-only planner harness naming.

## Required Corrections

- Add an explicit `planner-conversation` harness with backend `planner-model`.
- Resolve Planner Chat from the submitted or default `ProjectConfig`.
- Enforce read-only Planner Conversation Harness permissions.
- Keep deterministic Planner responses only in mock/test mode.
- Add `PlanDraft.memory_proposals` and display them without writing memory.
- Tighten long-term memory confirmation so only `planning_chat` can confirm it.
- Keep OpenHands payload projection and native fallback bounded.
