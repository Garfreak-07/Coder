import type {
  AgentModelTier,
  AgentWorkflowAgent,
  AgentWorkflowRole,
  CapabilitySpec
} from "../../types";

const agentModelTiers: AgentModelTier[] = ["best", "standard", "economy"];
const agentWorkflowRoles: AgentWorkflowRole[] = [
  "planner",
  "executor",
  "worker",
  "tester",
  "reviewer",
  "writer",
  "researcher",
  "summarizer",
  "custom"
];

interface AgentWorkflowAgentInspectorProps {
  agent: AgentWorkflowAgent;
  capabilities: CapabilitySpec[];
  onChange: (patch: Partial<AgentWorkflowAgent>) => void;
}

export function AgentWorkflowAgentInspector({
  agent,
  capabilities,
  onChange
}: AgentWorkflowAgentInspectorProps) {
  const selectedCapabilities = new Set(agent.capabilities);
  const visibleCapabilities = capabilities.filter(
    (capability) => capability.allowed_roles.includes(agent.role) || selectedCapabilities.has(capability.id)
  );

  function toggleCapability(capabilityId: string, checked: boolean) {
    const nextCapabilities = checked
      ? Array.from(new Set([...agent.capabilities, capabilityId]))
      : agent.capabilities.filter((candidate) => candidate !== capabilityId);
    onChange({ capabilities: nextCapabilities });
  }

  return (
    <div className="form-stack agent-editor">
      <div className="summary-grid">
        <span>{agent.role}</span>
        <span>{agent.can_talk_to_human ? "Can ask user" : "Does not ask user"}</span>
      </div>
      <label>
        Name
        <input value={agent.name} onChange={(event) => onChange({ name: event.target.value })} />
      </label>
      <label>
        Role
        <select value={agent.role} onChange={(event) => onChange({ role: event.target.value as AgentWorkflowRole })}>
          {agentWorkflowRoles.map((role) => (
            <option key={role} value={role}>
              {role}
            </option>
          ))}
        </select>
      </label>
      <label>
        Model Tier
        <select value={agent.model_tier} onChange={(event) => onChange({ model_tier: event.target.value as AgentModelTier })}>
          {agentModelTiers.map((tier) => (
            <option key={tier} value={tier}>
              {tier}
            </option>
          ))}
        </select>
      </label>
      <label className="checkbox-row">
        <input
          type="checkbox"
          checked={agent.can_talk_to_human}
          disabled={agent.role !== "planner"}
          onChange={(event) => onChange({ can_talk_to_human: event.target.checked })}
        />
        Allow asking the user (Planner only)
      </label>
      <div className="panel-subtitle">Capabilities</div>
      {capabilities.length === 0 ? (
        <div className="muted">Capability catalog is unavailable.</div>
      ) : (
        <div className="capability-list">
          {visibleCapabilities.map((capability) => {
            const selected = selectedCapabilities.has(capability.id);
            const roleAllowed = capability.allowed_roles.includes(agent.role);
            return (
              <label className={`capability-option ${selected ? "selected" : ""}`} key={capability.id}>
                <input
                  type="checkbox"
                  checked={selected}
                  disabled={!roleAllowed && !selected}
                  onChange={(event) => toggleCapability(capability.id, event.target.checked)}
                />
                <span>
                  <strong>{capability.label}</strong>
                  <small>{capability.description}</small>
                  <small>
                    Produces: {capability.produces.join(", ") || "none"} · Requires: {capability.requires.join(", ") || "none"}
                  </small>
                  <small>
                    Permissions: {capabilityPermissionSummary(capability)}
                    {capability.runtime_effects.length > 0 ? ` · Effects: ${capability.runtime_effects.join(", ")}` : ""}
                  </small>
                  {!roleAllowed && selected && <small className="warning-text">Not allowed for role {agent.role}</small>}
                </span>
              </label>
            );
          })}
        </div>
      )}
    </div>
  );
}

function capabilityPermissionSummary(capability: CapabilitySpec): string {
  const permissions = [
    capability.permissions.read_files ? "read files" : null,
    capability.permissions.edit_files ? "edit files" : null,
    capability.permissions.run_commands ? "run commands" : null,
    capability.permissions.use_network ? "network" : null
  ].filter(Boolean);
  return permissions.length > 0 ? permissions.join(", ") : "no elevated permissions";
}
