import type { AgentWorkflowValidationResult } from "../../types";

export function AgentWorkflowValidationPanel({ result }: { result: AgentWorkflowValidationResult | null }) {
  if (!result) return null;
  const statusTone = result.status === "pass" ? "good" : result.status === "warning" ? "warn" : "bad";
  return (
    <div className="validation-panel">
      <div className="event-heading">
        <span className={`status-pill ${statusTone}`}>{result.status}</span>
        <strong>Agent workflow validation</strong>
      </div>
      <div className="summary-grid">
        <span>{String(result.summary.agents ?? 0)} agents</span>
        <span>{String(result.summary.edges ?? 0)} edges</span>
        <span>Primary Planner: {String(result.summary.primary_planner_id ?? "none")}</span>
        <span>Max rounds: {String(result.summary.max_auto_rounds ?? "unset")}</span>
      </div>
      {result.issues.length === 0 ? (
        <div className="muted">No validation issues.</div>
      ) : (
        <div className="preflight-issues">
          {result.issues.map((issue, index) => (
            <div className={`preflight-issue ${issue.level}`} key={`${issue.code}-${issue.target_id ?? "workflow"}-${index}`}>
              <strong>{issue.message}</strong>
              <small>
                {issue.level.toUpperCase()} · {issue.code} · {issue.target_type}
                {issue.target_id ? `:${issue.target_id}` : ""}
              </small>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
