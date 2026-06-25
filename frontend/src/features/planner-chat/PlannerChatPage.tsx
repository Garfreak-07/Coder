import type { ReactNode } from "react";
import type { PlannerChatDraft } from "../../types";

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
  plannerStrength: PlannerStrength;
  workflowSummary: PlannerChatWorkflowSummary;
  onCancelDraft: () => void;
  onConfirmDraft: () => void;
  onDraftRequestTextChange: (value: string) => void;
  onDraftScopesTextChange: (value: string) => void;
  onDraftSuccessCriteriaTextChange: (value: string) => void;
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
  plannerStrength,
  workflowSummary,
  onCancelDraft,
  onConfirmDraft,
  onDraftRequestTextChange,
  onDraftScopesTextChange,
  onDraftSuccessCriteriaTextChange,
  onRepoChange,
  onRequestChange,
  onScopesTextChange,
  onPlannerStrengthChange,
  onSubmitRequest
}: PlannerChatPageProps) {
  const inputValue = request;
  const inputDisabled = runLoading || planDraft !== null;
  const canSend = request.trim().length > 0 && !inputDisabled;
  const canConfirmDraft = draftRequestText.trim().length > 0 && !runLoading;
  const statusMessage = planDraft ? "Plan ready for review" : formatRunStatus(runStatus);

  function submit() {
    if (!canSend) return;
    onSubmitRequest();
  }

  function confirmDraft() {
    if (!canConfirmDraft) return;
    onConfirmDraft();
  }

  return (
    <main className="chat-page">
      <section className="chat-thread" aria-label="Planner conversation">
        {!submittedRequest && !activeRunId && !planDraft ? (
          <div className="chat-empty">
            <h2>What should the Planner work on?</h2>
            <p>Send a request and the Planner will coordinate the Executor.</p>
          </div>
        ) : (
          <>
            {submittedRequest && (
              <article className="chat-message user-message">
                <div className="message-role">You</div>
                <p>{submittedRequest}</p>
              </article>
            )}
            <article className="chat-message planner-message">
              <div className="message-role">Planner</div>
              <div className="message-card">
                <div className="message-status">
                  <span>{statusMessage}</span>
                </div>
                {activeRunId && <p>The confirmed workflow is running. Results will appear here as they arrive.</p>}
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
            <button onClick={submit} disabled={!canSend}>
              {runLoading ? "Drafting..." : "Draft plan"}
            </button>
          </div>
        </div>
      </section>
    </main>
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
