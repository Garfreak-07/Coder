import type {
  AgentModelTier,
  AgentWorkflowAgent,
  AgentWorkflowRole,
  CapabilitySpec,
  RoleCardSpec
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
  roleCards: RoleCardSpec[];
  isPrimaryPlanner: boolean;
  onChange: (patch: Partial<AgentWorkflowAgent>) => void;
}

export function AgentWorkflowAgentInspector({
  agent,
  capabilities,
  roleCards,
  isPrimaryPlanner,
  onChange
}: AgentWorkflowAgentInspectorProps) {
  const selectedCapabilities = new Set(agent.capabilities);
  const visibleCapabilities = capabilities.filter(
    (capability) => capability.allowed_roles.includes(agent.role) || selectedCapabilities.has(capability.id)
  );
  const selectedRoleCard = roleCards.find((card) => card.id === agent.role_card) ?? null;

  function toggleCapability(capabilityId: string, checked: boolean) {
    const nextCapabilities = checked
      ? Array.from(new Set([...agent.capabilities, capabilityId]))
      : agent.capabilities.filter((candidate) => candidate !== capabilityId);
    onChange({ capabilities: nextCapabilities });
  }

  function applyRoleCard(roleCardId: string) {
    const roleCard = roleCards.find((card) => card.id === roleCardId);
    if (!roleCard) {
      onChange({ role_card: null });
      return;
    }
    onChange({
      role_card: roleCard.id,
      role: roleCard.role,
      capabilities: [...roleCard.default_capabilities],
      can_talk_to_human: false,
      model_tier: "standard"
    });
  }

  return (
    <div className="form-stack agent-editor">
      <div className="summary-grid">
        <span>{selectedRoleCard?.label ?? agent.role}</span>
        <span>{agent.can_talk_to_human ? "Can ask user" : "Does not ask user"}</span>
      </div>
      <label>
        Name
        <input value={agent.name} onChange={(event) => onChange({ name: event.target.value })} />
      </label>
      {isPrimaryPlanner ? (
        <div className="agent-policy-summary">
          <div className="panel-subtitle">Planner</div>
          <div className="muted">Primary Planner</div>
        </div>
      ) : (
        <label>
          Role
          <select value={agent.role_card ?? ""} onChange={(event) => applyRoleCard(event.target.value)}>
            <option value="">Custom</option>
            {roleCards.map((roleCard) => (
              <option key={roleCard.id} value={roleCard.id}>
                {roleCard.label}
              </option>
            ))}
          </select>
        </label>
      )}
      {selectedRoleCard && <div className="muted">{selectedRoleCard.description}</div>}

      <details className="json-details">
        <summary>Advanced Agent Internals</summary>
        <div className="form-stack">
          <label>
            Runtime Role
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
                        Produces: {capability.produces.join(", ") || "none"} / Requires: {capability.requires.join(", ") || "none"}
                      </small>
                      <small>
                        Permissions: {capabilityPermissionSummary(capability)}
                        {capability.runtime_effects.length > 0 ? ` / Effects: ${capability.runtime_effects.join(", ")}` : ""}
                      </small>
                      {!roleAllowed && selected && <small className="warning-text">Not allowed for role {agent.role}</small>}
                    </span>
                  </label>
                );
              })}
            </div>
          )}
        </div>
      </details>
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
