import type { ReactNode } from "react";

export type PlannerStrength = "fast" | "balanced" | "strong";

interface PlannerChatPageProps {
  activeRunId: string | null;
  evidence: ReactNode;
  repo: string;
  request: string;
  runLoading: boolean;
  runStatus: string;
  scopesText: string;
  submittedRequest: string;
  plannerStrength: PlannerStrength;
  onRepoChange: (value: string) => void;
  onRequestChange: (value: string) => void;
  onScopesTextChange: (value: string) => void;
  onPlannerStrengthChange: (value: PlannerStrength) => void;
  onSubmitRequest: () => void;
}

export function PlannerChatPage({
  activeRunId,
  evidence,
  repo,
  request,
  runLoading,
  runStatus,
  scopesText,
  submittedRequest,
  plannerStrength,
  onRepoChange,
  onRequestChange,
  onScopesTextChange,
  onPlannerStrengthChange,
  onSubmitRequest
}: PlannerChatPageProps) {
  const inputValue = request;
  const inputDisabled = runLoading;
  const canSend = request.trim().length > 0 && !inputDisabled;

  function submit() {
    if (!canSend) return;
    onSubmitRequest();
  }

  return (
    <main className="chat-page">
      <section className="chat-thread" aria-label="Planner conversation">
        {!submittedRequest && !activeRunId ? (
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
                  <span>{runStatus}</span>
                  {activeRunId && <code>{activeRunId.slice(0, 8)}</code>}
                </div>
                {activeRunId && <p>Running the Planner-led AgentGraph.</p>}
              </div>
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
              {runLoading ? "Sending..." : "Send"}
            </button>
          </div>
        </div>
      </section>
    </main>
  );
}
