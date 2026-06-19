export type NodeType = "start" | "agent" | "tool" | "condition" | "human_gate" | "end";

export interface PermissionPolicy {
  read_files: boolean;
  edit_files: boolean;
  run_commands: boolean;
  use_network: boolean;
  requires_approval: boolean;
}

export interface ContextPolicy {
  input_keys: string[];
  summary_keys: string[];
  max_items_per_key: number;
  max_chars_per_value: number;
  include_event_history: boolean;
  include_full_outputs: boolean;
}

export interface AgentSpec {
  id: string;
  name?: string | null;
  role: string;
  goal: string;
  instructions: string;
  provider?: string | null;
  model?: string | null;
  tools: string[];
  output_key?: string | null;
  permissions: PermissionPolicy;
  context: ContextPolicy;
}

export interface NodeSpec {
  id: string;
  type: NodeType;
  agent_id?: string | null;
  tool?: string | null;
  input?: Record<string, unknown>;
  output_key?: string | null;
  condition?: string | null;
  approval_reason?: string | null;
}

export interface EdgeSpec {
  from: string;
  to: string;
  when?: string | null;
  priority?: number;
  max_traversals?: number | null;
}

export interface WorkflowSpec {
  id: string;
  version: string;
  name: string;
  description: string;
  max_steps: number;
  max_agent_calls: number;
  max_tool_calls: number;
  token_budget: number | null;
  agents: AgentSpec[];
  nodes: NodeSpec[];
  edges: EdgeSpec[];
  stop_conditions: string[];
}

export interface WorkflowSummary {
  id: string;
  version?: string;
  name?: string;
  description?: string;
  nodes: number;
  edges: number;
  agents: number;
}

export interface AgentSummary {
  id: string;
  name?: string;
  role?: string;
  goal?: string;
  model?: string;
  tools: string[];
}

export interface LibraryIndex {
  agents: AgentSummary[];
  workflows: WorkflowSummary[];
}

export interface RunEvent {
  id?: string;
  type: string;
  node_id?: string | null;
  message?: string | null;
  payload?: Record<string, unknown>;
  created_at?: string;
}
