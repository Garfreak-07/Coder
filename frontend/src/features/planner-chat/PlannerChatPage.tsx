import type { ReactNode } from "react";
import type { PlannerChatDraft, PlannerChatSession, PlannerInteractionMode } from "../../types";

export type PlannerStrength = "fast" | "balanced" | "strong";

export interface PlannerChatWorkflowSummary {
  workflowName: string;
  plannerName: string;
  executorNames: string[];
  skillPackIds: string[];
  knowledgePackIds: string[];
  memoryPackIds: string[];
  maxAutoRounds: number | null;
}

interface PlannerChatPageProps {
  activeRunId: string | null;
  draftRequestText: string;
  draftScopesText: string;
  draftSuccessCriteriaText: string;
  evidence: ReactNode;
  repo: string;
  request: string;
  runLoading: boolean;
  runStatus: string;
  scopesText: string;
  submittedRequest: string;
  planDraft: PlannerChatDraft | null;
  plannerInteractionMode: PlannerInteractionMode;
  plannerSession: PlannerChatSession | null;
  plannerStrength: PlannerStrength;
  workflowSummary: PlannerChatWorkflowSummary;
  onCancelDraft: () => void;
  onConfirmDraft: () => void;
  onDraftPlan: () => void;
  onDraftRequestTextChange: (value: string) => void;
  onDraftScopesTextChange: (value: string) => void;
  onDraftSuccessCriteriaTextChange: (value: string) => void;
  onPlannerInteractionModeChange: (value: PlannerInteractionMode) => void;
  onRepoChange: (value: string) => void;
  onRequestChange: (value: string) => void;
  onScopesTextChange: (value: string) => void;
  onPlannerStrengthChange: (value: PlannerStrength) => void;
  onSubmitRequest: () => void;
}

export function PlannerChatPage({
  activeRunId,
  draftRequestText,
  draftScopesText,
  draftSuccessCriteriaText,
  evidence,
  repo,
  request,
  runLoading,
  runStatus,
  scopesText,
  submittedRequest,
  planDraft,
  plannerInteractionMode,
  plannerSession,
  plannerStrength,
  workflowSummary,
  onCancelDraft,
  onConfirmDraft,
  onDraftPlan,
  onDraftRequestTextChange,
  onDraftScopesTextChange,
  onDraftSuccessCriteriaTextChange,
  onPlannerInteractionModeChange,
  onRepoChange,
  onRequestChange,
  onScopesTextChange,
  onPlannerStrengthChange,
  onSubmitRequest
}: PlannerChatPageProps) {
  const inputValue = request;
  const inputDisabled = runLoading || planDraft !== null;
  const canSend = request.trim().length > 0 && !inputDisabled;
  const canDraft = request.trim().length > 0 && !runLoading && !plannerSession;
  const canConfirmDraft = draftRequestText.trim().length > 0 && !runLoading;
  const statusMessage = planDraft ? "Plan ready for review" : formatRunStatus(runStatus);
  const sessionMessages = plannerSession?.messages ?? [];
  const hasSessionMessages = sessionMessages.length > 0;

  function submit() {
    if (!canSend) return;
    onSubmitRequest();
  }

  function draftPlan() {
    if (!canDraft) return;
    onDraftPlan();
  }

  function confirmDraft() {
    if (!canConfirmDraft) return;
    onConfirmDraft();
  }

  return (
    <main className="chat-page">
      <section className="chat-thread" aria-label="Planner conversation">
        {!submittedRequest && !activeRunId && !planDraft && !hasSessionMessages ? (
          <div className="chat-empty">
            <h2>What should the Planner work on?</h2>
            <p>Send a request and the Planner will coordinate the Executor.</p>
          </div>
        ) : (
          <>
            {hasSessionMessages ? (
              sessionMessages.map((message, index) => (
                <article
                  key={`${message.created_at ?? index}-${message.role}`}
                  className={`chat-message ${message.role === "user" ? "user-message" : "planner-message"}`}
                >
                  <div className="message-role">{message.role === "user" ? "You" : "Planner"}</div>
                  <p>{message.content}</p>
                </article>
              ))
            ) : (
              submittedRequest && (
                <article className="chat-message user-message">
                  <div className="message-role">You</div>
                  <p>{submittedRequest}</p>
                </article>
              )
            )}
            <article className="chat-message planner-message">
              <div className="message-role">Planner</div>
              <div className="message-card">
                <div className="message-status">
                  <span>{statusMessage}</span>
                </div>
                {activeRunId && <p>The confirmed workflow is running. Results will appear here as they arrive.</p>}
                {plannerSession?.last_turn && (
                  <PlannerTaskStateSummary session={plannerSession} />
                )}
              </div>
              {planDraft && (
                <div className="plan-draft-card">
                  <div className="plan-draft-header">
                    <span>Review before run</span>
                  </div>
                  <p className="plan-draft-summary">{planDraft.summary}</p>
                  <DraftWorkflowSummary summary={workflowSummary} />
                  <div className="draft-edit-grid">
                    <label>
                      Request
                      <textarea
                        value={draftRequestText}
                        onChange={(event) => onDraftRequestTextChange(event.target.value)}
                        rows={4}
                      />
                    </label>
                    <label>
                      Scope
                      <textarea
                        placeholder="Whole project if left empty."
                        value={draftScopesText}
                        onChange={(event) => onDraftScopesTextChange(event.target.value)}
                        rows={3}
                      />
                    </label>
                    <label>
                      Success criteria
                      <textarea
                        value={draftSuccessCriteriaText}
                        onChange={(event) => onDraftSuccessCriteriaTextChange(event.target.value)}
                        rows={4}
                      />
                    </label>
                  </div>
                  <RiskList risks={planDraft.risks} />
                  <div className="draft-actions">
                    <button onClick={onCancelDraft} disabled={runLoading}>Discard</button>
                    <button className="primary-action" onClick={confirmDraft} disabled={!canConfirmDraft}>
                      {runLoading ? "Starting..." : "Confirm and run"}
                    </button>
                  </div>
                </div>
              )}
              {evidence}
            </article>
          </>
        )}
      </section>

      <section className="chat-composer" aria-label="Planner input">
        <details className="run-settings-popover">
          <summary>Run settings</summary>
          <div className="run-settings-grid">
            <label>
              Project path
              <input value={repo} onChange={(event) => onRepoChange(event.target.value)} />
            </label>
            <label>
              Limit edit scope
              <textarea
                placeholder="Optional, one repository-relative path per line."
                value={scopesText}
                onChange={(event) => onScopesTextChange(event.target.value)}
                rows={2}
              />
            </label>
          </div>
        </details>
        <div className="composer-shell">
          <textarea
            value={inputValue}
            disabled={inputDisabled}
            onChange={(event) => onRequestChange(event.target.value)}
            placeholder="Message the Planner..."
            rows={4}
          />
          <div className="composer-footer">
            <div className="mode-toggle" aria-label="Planner interaction mode">
              <button
                type="button"
                className={plannerInteractionMode === "discuss" ? "selected" : ""}
                onClick={() => onPlannerInteractionModeChange("discuss")}
              >
                Discuss
              </button>
              <button
                type="button"
                className={plannerInteractionMode === "work" ? "selected" : ""}
                onClick={() => onPlannerInteractionModeChange("work")}
              >
                Work
              </button>
            </div>
            <label className="strength-control">
              Planner strength
              <select
                value={plannerStrength}
                onChange={(event) => onPlannerStrengthChange(event.target.value as PlannerStrength)}
              >
                <option value="fast">Fast</option>
                <option value="balanced">Standard</option>
                <option value="strong">Strong</option>
              </select>
            </label>
            <button onClick={draftPlan} disabled={!canDraft}>
              Draft plan
            </button>
            <button className="primary-action" onClick={submit} disabled={!canSend}>
              {runLoading ? "Sending..." : "Send"}
            </button>
          </div>
        </div>
      </section>
    </main>
  );
}

function PlannerTaskStateSummary({ session }: { session: PlannerChatSession }) {
  const state = session.task_state;

  return (
    <div className="planner-state-card">
      <div className="planner-state-strip">
        <span>{session.interaction_mode === "work" ? "Work" : "Discuss"}</span>
        <span>{state.readiness.replaceAll("_", " ")}</span>
        <span>{state.success_criteria.length} criteria</span>
        <span>{state.open_questions.length} questions</span>
      </div>
      {state.goal && <p className="planner-goal">{state.goal}</p>}
      <PlannerStateList title="Open questions" items={state.open_questions} />
      <PlannerStateList title="Acceptance" items={state.success_criteria} />
      <PlannerStateList title="Risks" items={state.risks} />
      <MemoryProposalList proposals={state.memory_proposals} />
    </div>
  );
}

function PlannerStateList({ title, items }: { title: string; items: string[] }) {
  if (items.length === 0) return null;
  return (
    <div className="planner-state-list">
      <span>{title}</span>
      <ul>
        {items.slice(0, 4).map((item, index) => (
          <li key={`${title}-${index}`}>{item}</li>
        ))}
      </ul>
    </div>
  );
}

function MemoryProposalList({ proposals }: { proposals: PlannerChatSession["task_state"]["memory_proposals"] }) {
  if (proposals.length === 0) return null;
  return (
    <div className="planner-state-list memory-proposal-list">
      <span>Memory proposals</span>
      <ul>
        {proposals.slice(0, 3).map((proposal) => (
          <li key={`${proposal.scope}-${proposal.key}`}>
            <strong>{proposal.scope}: {proposal.key}</strong>
            <p>{proposal.content}</p>
            <small>{proposal.rationale}</small>
          </li>
        ))}
      </ul>
    </div>
  );
}

function DraftWorkflowSummary({ summary }: { summary: PlannerChatWorkflowSummary }) {
  const executors = summary.executorNames.length > 0 ? summary.executorNames.join(", ") : "Executor";
  const selectedPacks = [
    ...summary.skillPackIds.map((id) => ({ group: "Skill", id })),
    ...summary.knowledgePackIds.map((id) => ({ group: "Knowledge", id })),
    ...summary.memoryPackIds.map((id) => ({ group: "Memory", id }))
  ];

  return (
    <div className="draft-workflow-summary">
      <div className="draft-summary-grid">
        <div>
          <span>Selected workflow</span>
          <strong>{summary.workflowName}</strong>
        </div>
        <div>
          <span>Planner</span>
          <strong>{summary.plannerName}</strong>
        </div>
        <div>
          <span>Executor</span>
          <strong>{executors}</strong>
        </div>
        <div>
          <span>Run limit</span>
          <strong>{summary.maxAutoRounds ? `${summary.maxAutoRounds} rounds` : "Default"}</strong>
        </div>
      </div>
      <div className="draft-pack-summary">
        <span>Selected packs</span>
        {selectedPacks.length > 0 ? (
          <div className="draft-pack-list">
            {selectedPacks.map((pack) => (
              <code key={`${pack.group}-${pack.id}`}>{pack.group}: {pack.id}</code>
            ))}
          </div>
        ) : (
          <div className="muted">No selected skill, knowledge, or memory packs.</div>
        )}
      </div>
    </div>
  );
}

function RiskList({ risks }: { risks: string[] }) {
  const items = risks.length > 0 ? risks : ["No specific risks identified."];

  return (
    <div className="draft-risk-list">
      <div>Risks to check</div>
      <ul>
        {items.map((item, index) => (
          <li key={`risk-${index}`}>{item}</li>
        ))}
      </ul>
    </div>
  );
}

function formatRunStatus(status: string): string {
  if (status === "ready") return "Ready";
  if (status === "queued") return "Run queued";
  if (status === "running") return "Run active";
  if (status === "completed") return "Run completed";
  if (status === "blocked") return "Run blocked";
  if (status === "failed") return "Run failed";
  return status;
}
