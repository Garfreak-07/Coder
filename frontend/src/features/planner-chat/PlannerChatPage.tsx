import type { ReactNode } from "react";
import type { ChangeSet, PlannerChatSession, TimelineItem } from "../../types";
import { ReviewChangesCard } from "../review-changes/ReviewChangesCard";
import { WorkTimeline } from "../work-timeline/WorkTimeline";

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
  changeSets: ChangeSet[];
  debugEvidence: ReactNode;
  diffByChangeSetId: Record<string, string>;
  loadingChangeSetId: string | null;
  repo: string;
  request: string;
  runLoading: boolean;
  scopesText: string;
  submittedRequest: string;
  timelineItems: TimelineItem[];
  plannerSession: PlannerChatSession | null;
  plannerStrength: PlannerStrength;
  providerSetupRequired: boolean;
  providerSetupMessage: string;
  reviewStateError: string | null;
  onAcceptChangeSet: (changeSetId: string) => void;
  onOpenProviderSettings: () => void;
  onLoadChangeSetDiff: (changeSetId: string) => void;
  onRepoChange: (value: string) => void;
  onRequestChange: (value: string) => void;
  onScopesTextChange: (value: string) => void;
  onPlannerStrengthChange: (value: PlannerStrength) => void;
  onStartWork: () => void;
  onSubmitRequest: () => void;
  onUndoChangeSet: (changeSetId: string) => void;
}

export function PlannerChatPage({
  activeRunId,
  changeSets,
  debugEvidence,
  diffByChangeSetId,
  loadingChangeSetId,
  repo,
  request,
  runLoading,
  scopesText,
  submittedRequest,
  timelineItems,
  plannerSession,
  plannerStrength,
  providerSetupRequired,
  providerSetupMessage,
  reviewStateError,
  onAcceptChangeSet,
  onOpenProviderSettings,
  onLoadChangeSetDiff,
  onRepoChange,
  onRequestChange,
  onScopesTextChange,
  onPlannerStrengthChange,
  onStartWork,
  onSubmitRequest,
  onUndoChangeSet
}: PlannerChatPageProps) {
  const inputDisabled = runLoading || providerSetupRequired;
  const canSend = request.trim().length > 0 && !inputDisabled;
  const plannerTaskState = plannerSession?.task_state ?? null;
  const canStartWork =
    Boolean(plannerSession) &&
    !runLoading &&
    !providerSetupRequired &&
    plannerTaskState?.readiness === "ready_to_execute" &&
    !activeRunId;
  const sessionMessages = plannerSession?.messages ?? [];
  const hasSessionMessages = sessionMessages.length > 0;
  const runIdForTimeline = activeRunId ?? plannerSession?.run_id ?? null;

  function submit() {
    if (!canSend) return;
    onSubmitRequest();
  }

  function startWork() {
    if (!canStartWork) return;
    onStartWork();
  }

  return (
    <main className="chat-page">
      <section className="chat-thread" aria-label="Planner conversation">
        {!submittedRequest && !runIdForTimeline && !hasSessionMessages ? (
          <div className="chat-empty">
            {providerSetupRequired && (
              <ProviderSetupBanner
                message={providerSetupMessage}
                onOpenProviderSettings={onOpenProviderSettings}
              />
            )}
            <div className="chat-empty-panel">
              <span className="eyebrow">Planner Chat</span>
              <h2>What should the Planner work on?</h2>
              <p>Chat with the Planner, then start work when the plan is ready.</p>
            </div>
          </div>
        ) : (
          <>
            {providerSetupRequired && (
              <ProviderSetupBanner
                message={providerSetupMessage}
                onOpenProviderSettings={onOpenProviderSettings}
              />
            )}
            {hasSessionMessages ? (
              sessionMessages.map((message, index) => (
                <article
                  key={`${message.created_at ?? index}-${message.role}`}
                  className={`chat-message ${message.role === "user" ? "user-message" : "planner-message"}`}
                >
                  <div className="message-bubble">
                    <div className="message-role">{message.role === "user" ? "You" : "Planner"}</div>
                    <p>{message.content}</p>
                  </div>
                </article>
              ))
            ) : (
              submittedRequest && (
                <article className="chat-message user-message">
                  <div className="message-bubble">
                    <div className="message-role">You</div>
                    <p>{submittedRequest}</p>
                  </div>
                </article>
              )
            )}
            {runLoading && (
              <div className="chat-loading-row" role="status">
                <span className="loading-dot" aria-hidden="true" />
                Working...
              </div>
            )}
            {reviewStateError && (
              <div className="work-state-error" role="alert">
                <strong>Work results could not be loaded.</strong>
                <p>{reviewStateError}</p>
              </div>
            )}
            <WorkTimeline runId={runIdForTimeline} items={timelineItems} />
            <ReviewChangesCard
              changeSets={changeSets}
              diffByChangeSetId={diffByChangeSetId}
              loadingChangeSetId={loadingChangeSetId}
              onAccept={onAcceptChangeSet}
              onLoadDiff={onLoadChangeSetDiff}
              onUndo={onUndoChangeSet}
            />
            {debugEvidence}
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
            value={request}
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
            <div className="composer-actions">
              <button
                className={`start-work-action ${canStartWork ? "primary-action" : ""}`}
                onClick={startWork}
                disabled={!canStartWork}
              >
                {runLoading ? "Starting..." : "Start Work"}
              </button>
              <button className={canStartWork ? "" : "primary-action"} onClick={submit} disabled={!canSend}>
                {runLoading ? "Sending..." : "Send"}
              </button>
            </div>
          </div>
        </div>
      </section>
    </main>
  );
}

function ProviderSetupBanner({
  message,
  onOpenProviderSettings
}: {
  message: string;
  onOpenProviderSettings: () => void;
}) {
  return (
    <div className="provider-setup-card" role="status">
      <div>
        <strong>Provider setup required</strong>
        <p>{message}</p>
      </div>
      <button onClick={onOpenProviderSettings}>Open Settings</button>
    </div>
  );
}
