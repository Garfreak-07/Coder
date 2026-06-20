import type { AgentWorkflowAgent, AgentWorkflowEdge } from "../../types";

interface AgentWorkflowEdgeInspectorProps {
  edge: AgentWorkflowEdge;
  agents: AgentWorkflowAgent[];
  onChange: (patch: Partial<AgentWorkflowEdge>) => void;
}

export function AgentWorkflowEdgeInspector({
  edge,
  agents,
  onChange
}: AgentWorkflowEdgeInspectorProps) {
  const from = agents.find((agent) => agent.id === edge.from);
  const to = agents.find((agent) => agent.id === edge.to);
  return (
    <div className="form-stack">
      <div className="summary-grid">
        <span>{from?.name ?? edge.from}</span>
        <span>{to?.name ?? edge.to}</span>
      </div>
      <label>
        Label
        <input value={edge.label ?? ""} onChange={(event) => onChange({ label: event.target.value || null })} />
      </label>
      <label className="checkbox-row">
        <input type="checkbox" checked={Boolean(edge.loop)} onChange={(event) => onChange({ loop: event.target.checked })} />
        This edge loops back to the Planner
      </label>
      <div className="muted">Handoff is inferred from selected capabilities during validation and compile.</div>
    </div>
  );
}
