export type NodeType = "start" | "agent" | "tool" | "mcp_tool" | "condition" | "loop" | "human_gate" | "end";
export type LoopMode = "while" | "for_each" | "retry_until";

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
  include_all_state: boolean;
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
  artifact_type?: "plan_artifact" | "patch_artifact" | "review_artifact" | null;
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
  loop_mode?: LoopMode | null;
  items_key?: string | null;
  item_key?: string | null;
  iteration_key?: string | null;
  max_iterations?: number | null;
  collect_key?: string | null;
  summary_key?: string | null;
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

export interface RunSummaryItem {
  id: string;
  workflow_id: string;
  repo_root: string;
  request: string;
  status: string;
  events: number;
  agent_calls?: number;
  tool_calls?: number;
  estimated_tokens_used?: number;
  stored_run_id?: string | null;
  error?: string | null;
}

export interface RunResult {
  status: string;
  data: Record<string, unknown>;
  summaries: Record<string, string>;
  events: RunEvent[];
  estimated_tokens_used: number;
  agent_calls: number;
  tool_calls: number;
  blocked_node_id?: string | null;
  resume_checkpoint?: Record<string, unknown> | null;
}

export interface StoredRunDetail {
  id: string;
  workflow_id: string;
  repo_root: string;
  request: string;
  result: RunResult;
}

export interface RunEventsPage {
  events: RunEvent[];
  cursor: number;
  next_cursor: number;
  has_more: boolean;
}

export interface ContextPacketDetail {
  packet_id: string;
  packet: Record<string, unknown>;
}

export interface ArtifactDetail {
  artifact_id: string;
  artifact: Record<string, unknown>;
}

export interface ToolResultDetail {
  tool_result_id: string;
  result: Record<string, unknown>;
}

export interface BlobDetail {
  blob_id: string;
  size_bytes: number;
  media_type: string;
  content: string;
}

export interface LiveRunDetail {
  id: string;
  workflow_id: string;
  repo_root: string;
  request: string;
  status: string;
  events: RunEvent[];
  result?: RunResult | null;
  stored_run_id?: string | null;
  error?: string | null;
}

export interface HealthStatus {
  status: string;
  tools: string[];
}
