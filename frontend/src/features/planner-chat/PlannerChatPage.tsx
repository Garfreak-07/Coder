import type { ReactNode } from "react";
import type { ChangeSet, PlannerArtifact, PlannerChatMessage, PlannerChatSession, TimelineItem } from "../../types";
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
  plannerLoading: boolean;
  scopesText: string;
  submittedRequest: string;
  timelineItems: TimelineItem[];
  workflowRunning: boolean;
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
  onCancelWork: () => void;
  onCheckWorkStatus: () => void;
  onSubmitGuidance: () => void;
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
  plannerLoading,
  scopesText,
  submittedRequest,
  timelineItems,
  workflowRunning,
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
  onCancelWork,
  onCheckWorkStatus,
  onSubmitGuidance,
  onStartWork,
  onSubmitRequest,
  onUndoChangeSet
}: PlannerChatPageProps) {
  const inputDisabled = plannerLoading || providerSetupRequired;
  const canSend = request.trim().length > 0 && !inputDisabled;
  const plannerTaskState = plannerSession?.task_state ?? null;
  const canStartWork =
    Boolean(plannerSession) &&
    !plannerLoading &&
    !workflowRunning &&
    !providerSetupRequired &&
    plannerTaskState?.readiness === "ready_to_execute";
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
                  <PlannerMessageBubble message={message} />
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
            {plannerLoading && (
              <div className="chat-loading-row" role="status">
                <span className="loading-dot" aria-hidden="true" />
                Planner is responding...
              </div>
            )}
            {workflowRunning && (
              <div className="chat-loading-row workflow-running-row" role="status">
                <span className="loading-dot" aria-hidden="true" />
                Workflow is running. Planner Chat remains available.
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
              {workflowRunning ? (
                <>
                  <button onClick={onCheckWorkStatus} disabled={plannerLoading}>
                    Check status
                  </button>
                  <button onClick={onSubmitGuidance} disabled={!canSend}>
                    Guide work
                  </button>
                  <button className="stop-work-action" onClick={onCancelWork} disabled={plannerLoading}>
                    Stop work
                  </button>
                </>
              ) : (
                <button
                  className={`start-work-action ${canStartWork ? "primary-action" : ""}`}
                  onClick={startWork}
                  disabled={!canStartWork}
                >
                  Start Work
                </button>
              )}
              <button className={canStartWork ? "" : "primary-action"} onClick={submit} disabled={!canSend}>
                {plannerLoading ? "Sending..." : "Send"}
              </button>
            </div>
          </div>
        </div>
      </section>
    </main>
  );
}

function PlannerMessageBubble({ message }: { message: PlannerChatMessage }) {
  return (
    <div className="message-bubble">
      <div className="message-role">{message.role === "user" ? "You" : "Planner"}</div>
      <p>{message.content}</p>
      {message.response_truncated && (
        <div className="planner-truncation-notice" role="status">
          Planner summarized a long response. Ask for details if needed.
        </div>
      )}
      <PlannerArtifacts artifacts={message.artifacts} largeArtifacts={message.large_artifacts} />
    </div>
  );
}

function PlannerArtifacts({
  artifacts,
  largeArtifacts
}: {
  artifacts?: PlannerArtifact[];
  largeArtifacts?: boolean;
}) {
  const safeArtifacts = Array.isArray(artifacts) ? artifacts : [];
  if (safeArtifacts.length === 0) return null;
  return (
    <div className="planner-artifacts">
      {safeArtifacts.map((artifact, index) => (
        <PlannerArtifactCard
          artifact={artifact}
          collapsed={Boolean(largeArtifacts || artifact.collapsed)}
          key={`${artifact.type}-${artifact.title}-${index}`}
        />
      ))}
    </div>
  );
}

function PlannerArtifactCard({
  artifact,
  collapsed
}: {
  artifact: PlannerArtifact;
  collapsed: boolean;
}) {
  return (
    <details className="planner-artifact-card" open={!collapsed}>
      <summary>{artifact.title}</summary>
      {artifact.type === "table" && <PlannerTableArtifact artifact={artifact} />}
      {artifact.type === "notes" && (
        <ul>
          {artifact.items.map((item, index) => (
            <li key={`${artifact.title}-${index}`}>{item}</li>
          ))}
        </ul>
      )}
      {artifact.type === "text" && <p>{artifact.content}</p>}
    </details>
  );
}

function PlannerTableArtifact({
  artifact
}: {
  artifact: Extract<PlannerArtifact, { type: "table" }>;
}) {
  return (
    <div className="planner-artifact-table-wrap">
      <table>
        <thead>
          <tr>
            {artifact.columns.map((column, index) => (
              <th key={`${column}-${index}`}>{column}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {artifact.rows.map((row, rowIndex) => (
            <tr key={`${artifact.title}-${rowIndex}`}>
              {artifact.columns.map((_, columnIndex) => (
                <td key={`${artifact.title}-${rowIndex}-${columnIndex}`}>{row[columnIndex] ?? ""}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
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
