import type {
  AgentWorkflowSpec,
  AgentWorkflowSummary,
  ArtifactDetail,
  BlobDetail,
  CapabilitySpec,
  HealthStatus,
  LibraryIndex,
  RoleCardSpec,
  RunEvent,
  RunEventsPage,
  RunSummaryItem,
  RustCoderEvent,
  RustFinalReport,
  RustProjectConfig,
  RustRunDetail,
  RustRunEventsResponse,
  RustRunReportResponse,
  RustRunSummary,
  StoredRunDetail
} from "./types";
import {
  legacyCanvasToWorkflowExport,
  parseWorkflowImport,
  rustValidationReportToAgentWorkflowValidationResult,
  workflowSpecToLegacyCanvas
} from "./workflowSpecAdapter";
import { normalizeAgentWorkflow } from "./workflowGraph";

export interface RustHealthResponse {
  status: string;
  service?: string;
  api_version?: string;
}

export interface RustCapabilitiesResponse {
  api_version: string;
  workflow: string[];
  runs: string[];
  tools: string[];
  planner_chat: string[];
  settings: string[];
  extensions: string[];
  memory: string[];
}

export interface RustDefaultWorkflowResponse {
  workflow_id: string;
  config: RustProjectConfig;
  workflow?: unknown;
}

export interface RustLibraryWorkflowSummary {
  id: string;
  workflow: unknown;
}

export interface RustLibraryResponse {
  workflows: RustLibraryWorkflowSummary[];
}

export interface RustLibraryWorkflowSaveRequest {
  workflow_id: string;
  workflow: unknown;
}

export interface RustLibraryWorkflowGetResponse {
  workflow_id: string;
  workflow: unknown;
}

export function rustHealthToHealthStatus(response: RustHealthResponse): HealthStatus {
  return {
    status: response.status,
    tools: response.service ? [response.service] : []
  };
}

export function rustCapabilitiesToCapabilitySpecs(response: RustCapabilitiesResponse): CapabilitySpec[] {
  const ids = uniqueStrings([
    ...response.workflow,
    ...response.runs,
    ...response.tools,
    ...response.planner_chat,
    ...response.settings,
    ...response.extensions,
    ...response.memory
  ]);
  return ids.map((id) => ({
    id,
    label: labelForCapability(id),
    description: "Rust API v3 capability.",
    allowed_roles: ["planner", "executor"],
    requires: [],
    produces: [],
    permissions: {
      read_files: id.includes("read") || id.includes("repo") || id.includes("git"),
      edit_files: id.includes("patch_apply"),
      run_commands: id.includes("command"),
      use_network: false
    },
    can_talk_to_human: id.includes("planner"),
    runtime_effects: []
  }));
}

export function rustRoleCardsToRoleCards(response: { role_cards: RoleCardSpec[] }): RoleCardSpec[] {
  return response.role_cards;
}

export function rustDefaultWorkflowToAgentWorkflow(response: RustDefaultWorkflowResponse): AgentWorkflowSpec {
  return workflowSpecToLegacyCanvas(response.config, response.workflow_id);
}

export function rustLibraryToLibraryIndex(response: RustLibraryResponse): LibraryIndex {
  return {
    agents: [],
    agent_workflows: response.workflows.map((item) => workflowSummaryFromUnknown(item.id, item.workflow))
  };
}

export function agentWorkflowToRustLibrarySaveRequest(workflow: AgentWorkflowSpec): RustLibraryWorkflowSaveRequest {
  const normalized = normalizeAgentWorkflow(workflow);
  return {
    workflow_id: normalized.id,
    workflow: legacyCanvasToWorkflowExport(normalized)
  };
}

export function rustLibraryWorkflowToAgentWorkflow(response: RustLibraryWorkflowGetResponse): AgentWorkflowSpec {
  return parseWorkflowImport(response.workflow);
}

export function rustRunSummaryToRunSummaryItem(summary: RustRunSummary): RunSummaryItem {
  return {
    id: summary.run_id,
    workflow_id: summary.metadata?.workflow_id ?? "unknown",
    repo_root: "",
    request: "",
    status: summary.metadata?.status ?? (summary.has_report ? "completed" : "unknown"),
    events: summary.event_count,
    agent_calls: 0,
    tool_calls: summary.repo_evidence_count,
    stored_run_id: summary.run_id
  };
}

export function rustRunDetailToStoredRunDetail(detail: RustRunDetail): StoredRunDetail {
  const events = detail.events.map(rustCoderEventToRunEvent);
  const status = detail.report?.status ?? detail.metadata?.status ?? "unknown";
  return {
    id: detail.run_id,
    workflow_id: detail.metadata?.workflow_id ?? "unknown",
    repo_root: "",
    request: "",
    result: {
      status,
      data: {
        final_report: detail.report ?? null,
        repo_evidence_count: detail.repo_evidence_count
      },
      summaries: detail.report?.summary ? { summary: detail.report.summary } : {},
      events,
      estimated_tokens_used: 0,
      agent_calls: 0,
      tool_calls: detail.repo_evidence_count
    }
  };
}

export function rustRunEventsToRunEventsPage(
  response: RustRunEventsResponse,
  cursor = 0,
  limit = 200
): RunEventsPage {
  const events = response.events.map(rustCoderEventToRunEvent);
  if (hasServerEventPagination(response)) {
    const nextCursor = response.next_after_sequence ?? cursor + events.length;
    return {
      events,
      cursor,
      next_cursor: nextCursor,
      has_more: response.truncated ?? nextCursor < (response.event_count ?? nextCursor)
    };
  }
  const page = events.slice(cursor, cursor + limit);
  const nextCursor = Math.min(cursor + page.length, events.length);
  return {
    events: page,
    cursor,
    next_cursor: nextCursor,
    has_more: nextCursor < events.length
  };
}

function hasServerEventPagination(response: RustRunEventsResponse): boolean {
  return (
    typeof response.event_count === "number" ||
    typeof response.returned_count === "number" ||
    typeof response.truncated === "boolean" ||
    response.next_after_sequence !== undefined
  );
}

export function rustCoderEventToRunEvent(event: RustCoderEvent): RunEvent {
  const payload = objectValue(event.payload);
  return {
    id: event.event_id,
    type: event.kind,
    node_id: stringOrNull(payload?.node_id),
    message: messageFromRustEvent(event.kind, payload),
    payload: payload ?? undefined,
    created_at: event.timestamp
  };
}

export function rustRunReportToArtifactDetail(response: RustRunReportResponse): ArtifactDetail {
  return {
    artifact_id: response.report_ref ?? "final-report.json",
    artifact: rustFinalReportToLegacyArtifact(response.report)
  };
}

export function rustArtifactPayloadToArtifactDetail(artifactId: string, payload: unknown): ArtifactDetail {
  return {
    artifact_id: artifactId,
    artifact: objectValue(payload) ?? { value: payload }
  };
}

export async function rustBlobToBlobDetail(blobId: string, blob: Blob): Promise<BlobDetail> {
  return {
    blob_id: blobId,
    size_bytes: blob.size,
    media_type: blob.type || "application/octet-stream",
    content: await blob.text()
  };
}

export { rustValidationReportToAgentWorkflowValidationResult };

function workflowSummaryFromUnknown(id: string, value: unknown): AgentWorkflowSummary {
  try {
    const workflow = parseWorkflowImport(value);
    return {
      id: workflow.id || id,
      version: workflow.version,
      name: workflow.name,
      description: workflow.description,
      agents: workflow.agents.length,
      edges: workflow.edges.length,
      max_auto_rounds: workflow.loop_policy.max_auto_rounds
    };
  } catch {
    const record = objectValue(value);
    const workflow = objectValue(record?.workflow) ?? record;
    const nodes = Array.isArray(workflow?.nodes) ? workflow.nodes.length : 0;
    const edges = Array.isArray(workflow?.edges) ? workflow.edges.length : 0;
    return {
      id,
      name: typeof workflow?.name === "string" ? workflow.name : id,
      description: "",
      agents: nodes,
      edges,
      max_auto_rounds: typeof workflow?.max_rounds === "number" ? workflow.max_rounds : null
    };
  }
}

function rustFinalReportToLegacyArtifact(report: RustFinalReport): Record<string, unknown> {
  return {
    artifact_type: "final_report",
    status: report.status,
    summary: report.summary,
    files: {
      modified: report.changed_files,
      created: [],
      deleted: []
    },
    checks: report.checks.map((check) => ({
      status: check,
      summary: check
    })),
    blocked_by: report.blockers,
    failed_by: report.status === "failed" ? report.blockers : [],
    evidence_refs: report.evidence_refs.map((ref) => ref.reference),
    patch_refs: report.patch_refs,
    artifact_refs: report.artifact_refs,
    next_steps: report.next_steps
  };
}

function messageFromRustEvent(kind: string, payload: Record<string, unknown> | null): string | null {
  if (typeof payload?.summary === "string") return payload.summary;
  if (typeof payload?.status === "string") return payload.status;
  return kind;
}

function labelForCapability(id: string): string {
  return id
    .split("_")
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(" ");
}

function uniqueStrings(values: string[]): string[] {
  return [...new Set(values)];
}

function stringOrNull(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function objectValue(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : null;
}
