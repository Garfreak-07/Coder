import { useCallback, useEffect, useMemo, useState } from "react";
import {
  addEdge,
  Background,
  Controls,
  MiniMap,
  ReactFlow,
  applyEdgeChanges,
  applyNodeChanges,
  type Connection,
  type Edge as FlowEdge,
  type EdgeChange,
  type Node as FlowNode,
  type NodeChange
} from "@xyflow/react";
import {
  approveLiveRun,
  getBlob,
  getArtifact,
  getContextPacket,
  getHealth,
  getAgent,
  getLibrary,
  getLiveRun,
  getLiveRuns,
  getProviderSettings,
  getProviderStatus,
  getRun,
  getRunEvents,
  getRuns,
  getToolResult,
  getWorkflow,
  rollbackPatch,
  saveAgent,
  saveProviderSettings,
  saveWorkflow,
  startLiveRun,
  subscribeRunEvents,
  testProvider,
  validateWorkflow
} from "./api";
import { codingWorkbenchWorkflow } from "./examples";
import { nodeTypeDescriptions, nodeTypeLabels, zhCN } from "./i18n";
import { instantiateWorkflowTemplate, workflowTemplate, workflowTemplateCards, type WorkflowTemplateCard } from "./template";
import type {
  AgentSpec,
  EdgeSpec,
  HealthStatus,
  LibraryIndex,
  LiveRunDetail,
  LoopMode,
  NodeSpec,
  NodeType,
  PreflightResult,
  ProviderSettings,
  ProviderStatus,
  ProviderStatusItem,
  RunEvent,
  RunSummaryItem,
  StoredRunDetail,
  WorkflowSpec
} from "./types";

const nodeTypes: NodeType[] = ["start", "agent", "tool", "mcp_tool", "condition", "loop", "human_gate", "end"];
const loopModes: LoopMode[] = ["retry_until", "while", "for_each"];
const t = zhCN;

interface PendingPreflightRun {
  repo: string;
  request: string;
  workflow: WorkflowSpec;
  approved: boolean;
  scopes: string[];
  result: PreflightResult;
}

interface PreflightToolFact {
  nodeId: string;
  name: string;
  risk: string;
  permissions: string[];
  requiresApproval: boolean;
}

interface ProviderFormState {
  default_provider: string;
  default_model: string;
  base_url: string;
  api_key: string;
  mock_mode: boolean;
}

export function App() {
  const [library, setLibrary] = useState<LibraryIndex>({ agents: [], workflows: [] });
  const [workflow, setWorkflow] = useState<WorkflowSpec>(workflowTemplate);
  const [jsonText, setJsonText] = useState(() => formatJson(workflowTemplate));
  const [nodes, setNodes] = useState<FlowNode[]>(() => toFlowNodes(workflowTemplate));
  const [edges, setEdges] = useState<FlowEdge[]>(() => toFlowEdges(workflowTemplate));
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>("start");
  const [status, setStatus] = useState(t.app.defaultStatus);
  const [repo, setRepo] = useState(".");
  const [scopesText, setScopesText] = useState("");
  const [request, setRequest] = useState("Inspect this project and propose the next safe step.");
  const [approved, setApproved] = useState(false);
  const [events, setEvents] = useState<RunEvent[]>([]);
  const [eventCursor, setEventCursor] = useState(0);
  const [eventHasMore, setEventHasMore] = useState(false);
  const [eventsLoadingMore, setEventsLoadingMore] = useState(false);
  const [activeRunId, setActiveRunId] = useState<string | null>(null);
  const [selectedEdgeId, setSelectedEdgeId] = useState<string | null>(null);
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null);
  const [runHistory, setRunHistory] = useState<RunSummaryItem[]>([]);
  const [liveRuns, setLiveRuns] = useState<RunSummaryItem[]>([]);
  const [health, setHealth] = useState<HealthStatus | null>(null);
  const [providerSettings, setProviderSettings] = useState<ProviderSettings | null>(null);
  const [providerStatus, setProviderStatus] = useState<ProviderStatus | null>(null);
  const [providerForm, setProviderForm] = useState<ProviderFormState>({
    default_provider: "openai",
    default_model: "gpt-4.1-mini",
    base_url: "",
    api_key: "",
    mock_mode: true
  });
  const [selectedRunDetail, setSelectedRunDetail] = useState<StoredRunDetail | LiveRunDetail | null>(null);
  const [selectedRunKind, setSelectedRunKind] = useState<"live" | "stored" | null>(null);
  const [pendingPreflight, setPendingPreflight] = useState<PendingPreflightRun | null>(null);
  const [preflightLoading, setPreflightLoading] = useState(false);

  const selectedNode = useMemo(
    () => workflow.nodes.find((node) => node.id === selectedNodeId) ?? null,
    [selectedNodeId, workflow.nodes]
  );
  const selectedEdge = useMemo(() => {
    if (!selectedEdgeId) return null;
    const edgeIndex = edgeIndexFromId(selectedEdgeId);
    return edgeIndex === null ? null : workflow.edges[edgeIndex] ?? null;
  }, [selectedEdgeId, workflow.edges]);
  const selectedAgent = useMemo(
    () => workflow.agents.find((agent) => agent.id === selectedAgentId) ?? null,
    [selectedAgentId, workflow.agents]
  );

  useEffect(() => {
    refreshLibrary();
    refreshRuntimeInfo();
    refreshProviderInfo();
  }, []);

  function refreshLibrary() {
    getLibrary()
      .then(setLibrary)
      .catch((error) => setStatus(`Failed to load library: ${error.message}`));
  }

  function refreshRuntimeInfo() {
    Promise.all([getRuns(), getLiveRuns(), getHealth()])
      .then(([runs, live, nextHealth]) => {
        setRunHistory(runs);
        setLiveRuns(live);
        setHealth(nextHealth);
      })
      .catch((error) => setStatus(`Failed to load runtime info: ${error.message}`));
  }

  function refreshProviderInfo() {
    Promise.all([getProviderSettings(), getProviderStatus()])
      .then(([settings, status]) => {
        setProviderSettings(settings);
        setProviderStatus(status);
        const provider = settings.default_provider || status.default_provider || "openai";
        setProviderForm({
          default_provider: provider,
          default_model: settings.default_model || status.default_model || "gpt-4.1-mini",
          base_url: settings.base_urls[provider] ?? "",
          api_key: "",
          mock_mode: settings.mock_mode
        });
      })
      .catch((error) => setStatus(`Failed to load provider settings: ${error.message}`));
  }

  async function persistProviderSettings() {
    const provider = providerForm.default_provider.trim().toLowerCase() || "openai";
    const baseUrls = { ...(providerSettings?.base_urls ?? {}) };
    if (providerForm.base_url.trim()) {
      baseUrls[provider] = providerForm.base_url.trim();
    } else {
      delete baseUrls[provider];
    }
    const payload: Record<string, unknown> = {
      default_provider: provider,
      default_model: providerForm.default_model.trim() || "gpt-4.1-mini",
      base_urls: baseUrls,
      mock_mode: providerForm.mock_mode
    };
    if (providerForm.api_key.trim()) {
      payload.api_keys = { [provider]: providerForm.api_key.trim() };
    }
    setStatus(`Saving provider ${provider}...`);
    try {
      const result = await saveProviderSettings(payload);
      setProviderSettings(result.settings);
      setProviderStatus(result.status);
      setProviderForm((current) => ({ ...current, default_provider: provider, api_key: "" }));
      setStatus(`Provider ${provider} saved.`);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  async function runProviderTest() {
    const provider = providerForm.default_provider.trim().toLowerCase() || "openai";
    setStatus(`Checking provider ${provider}...`);
    try {
      const result = await testProvider(provider);
      setProviderStatus(result);
      const item = result.providers[0] ?? result.default_status;
      setStatus(`Provider ${provider}: ${item.mode}, credentials ${item.credential_source}`);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  async function openStoredRun(runId: string) {
    setStatus(`Loading stored run ${runId}...`);
    try {
      const detail = await getRun(runId, false);
      const eventPage = await getRunEvents(runId);
      const hydrated = {
        ...detail,
        result: {
          ...detail.result,
          events: eventPage.events
        }
      };
      setSelectedRunKind("stored");
      setSelectedRunDetail(hydrated);
      setActiveRunId(null);
      setEvents(eventPage.events);
      setEventCursor(eventPage.next_cursor);
      setEventHasMore(eventPage.has_more);
      setEventsLoadingMore(false);
      setRepo(detail.repo_root);
      setRequest(detail.request);
      setStatus(
        eventPage.has_more
          ? `Stored run ${runId}: ${detail.result.status} (${eventPage.events.length}+ events)`
          : `Stored run ${runId}: ${detail.result.status}`
      );
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  async function loadMoreStoredEvents() {
    if (selectedRunKind !== "stored" || !selectedRunDetail || eventsLoadingMore || !eventHasMore) {
      return;
    }
    const runId = selectedRunDetail.id;
    setEventsLoadingMore(true);
    try {
      const eventPage = await getRunEvents(runId, eventCursor);
      setEvents((current) => mergeEvents(current, eventPage.events));
      setSelectedRunDetail((current) => {
        if (!current || current.id !== runId || selectedRunKind !== "stored") return current;
        const stored = current as StoredRunDetail;
        return {
          ...stored,
          result: {
            ...stored.result,
            events: mergeEvents(stored.result.events, eventPage.events)
          }
        };
      });
      setEventCursor(eventPage.next_cursor);
      setEventHasMore(eventPage.has_more);
      setStatus(`Stored run ${runId}: loaded ${eventPage.next_cursor} events`);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    } finally {
      setEventsLoadingMore(false);
    }
  }

  async function openLiveRun(runId: string, attach = false) {
    setStatus(`Loading live run ${runId}...`);
    try {
      const detail = await getLiveRun(runId);
      setSelectedRunKind("live");
      setSelectedRunDetail(detail);
      setActiveRunId(detail.id);
      setEvents(detail.events);
      setEventCursor(0);
      setEventHasMore(false);
      setEventsLoadingMore(false);
      setRepo(detail.repo_root);
      setRequest(detail.request);
      setStatus(`Live run ${runId}: ${detail.status}`);
      if (attach || detail.status === "queued" || detail.status === "running") {
        subscribeToRun(detail.id, `/api/v2/live-runs/${detail.id}/events`);
      }
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  function setCurrentWorkflow(next: WorkflowSpec) {
    setWorkflow(next);
    setJsonText(formatJson(next));
    setNodes(toFlowNodes(next));
    setEdges(toFlowEdges(next));
    setSelectedNodeId(next.nodes[0]?.id ?? null);
    setSelectedEdgeId(null);
    setSelectedAgentId(next.agents[0]?.id ?? null);
  }

  function useTemplateCard(template: WorkflowTemplateCard) {
    const next = instantiateWorkflowTemplate(template);
    setCurrentWorkflow(next);
    setStatus(`已从模板创建：${next.name}`);
  }

  async function loadWorkflow(workflowId: string) {
    setStatus(`Loading ${workflowId}...`);
    try {
      setCurrentWorkflow(await getWorkflow(workflowId));
      setStatus(`Loaded ${workflowId}`);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  function applyJson() {
    try {
      const parsed = JSON.parse(jsonText) as WorkflowSpec;
      setCurrentWorkflow(parsed);
      setStatus("JSON applied locally. Save to persist it.");
    } catch (error) {
      setStatus(error instanceof Error ? `Invalid JSON: ${error.message}` : "Invalid JSON");
    }
  }

  async function persistWorkflow() {
    try {
      const parsed = JSON.parse(jsonText) as WorkflowSpec;
      const saved = await saveWorkflow(parsed);
      setCurrentWorkflow(saved);
      refreshLibrary();
      setStatus(`Saved workflow ${saved.id}`);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  function exportWorkflow() {
    try {
      const parsed = JSON.parse(jsonText) as WorkflowSpec;
      downloadJson(`${parsed.id || "workflow"}.json`, parsed);
      setStatus(`Exported ${parsed.id || "workflow"}`);
    } catch (error) {
      setStatus(error instanceof Error ? `Cannot export invalid JSON: ${error.message}` : "Cannot export invalid JSON");
    }
  }

  function importWorkflow(file: File | null) {
    if (!file) return;
    file
      .text()
      .then((text) => {
        const parsed = JSON.parse(text) as WorkflowSpec;
        setCurrentWorkflow(parsed);
        setStatus(`Imported ${parsed.id}`);
      })
      .catch((error) => setStatus(error instanceof Error ? `Import failed: ${error.message}` : "Import failed"));
  }

  function updateWorkflow(mutator: (current: WorkflowSpec) => WorkflowSpec) {
    const next = mutator(workflow);
    setWorkflow(next);
    setJsonText(formatJson(next));
    setNodes(toFlowNodes(next));
    setEdges(toFlowEdges(next));
  }

  function addWorkflowNode(type: NodeType) {
    const id = uniqueNodeId(workflow, type);
    updateWorkflow((current) => ({
      ...current,
      nodes: [
        ...current.nodes,
        {
          id,
          type,
          ...(type === "agent" ? { agent_id: current.agents[0]?.id ?? "agent_id" } : {}),
          ...(type === "tool" ? { tool: "project_index" } : {}),
          ...(type === "mcp_tool" ? { tool: "tool_name", input: { server_command: "" } } : {}),
          ...(type === "condition" ? { condition: "state.value == True" } : {}),
          ...(type === "loop" ? { loop_mode: "retry_until" as LoopMode, condition: "review.status == 'done'", max_iterations: 3 } : {})
        }
      ]
    }));
    setSelectedNodeId(id);
  }

  function updateSelectedNode(patch: Partial<NodeSpec>) {
    if (!selectedNode) return;
    updateWorkflow((current) => ({
      ...current,
      nodes: current.nodes.map((node) => (node.id === selectedNode.id ? cleanNode({ ...node, ...patch }) : node)),
      edges: current.edges.map((edge) => ({
        ...edge,
        from: edge.from === selectedNode.id && patch.id ? patch.id : edge.from,
        to: edge.to === selectedNode.id && patch.id ? patch.id : edge.to
      }))
    }));
    if (patch.id) setSelectedNodeId(patch.id);
  }

  function updateSelectedEdge(patch: Partial<EdgeSpec>) {
    if (!selectedEdgeId) return;
    const edgeIndex = edgeIndexFromId(selectedEdgeId);
    if (edgeIndex === null) return;
    updateWorkflow((current) => ({
      ...current,
      edges: current.edges.map((edge, index) => (index === edgeIndex ? cleanEdge({ ...edge, ...patch }) : edge))
    }));
    setSelectedEdgeId(edgeIdFromIndex(edgeIndex));
  }

  function addAgent() {
    const agent = createDefaultAgent(uniqueAgentId(workflow));
    updateWorkflow((current) => ({
      ...current,
      agents: [...current.agents, agent]
    }));
    setSelectedAgentId(agent.id);
  }

  function updateSelectedAgent(patch: Partial<AgentSpec>) {
    if (!selectedAgent) return;
    const nextId = patch.id;
    updateWorkflow((current) => ({
      ...current,
      agents: current.agents.map((agent) => (agent.id === selectedAgent.id ? cleanAgent({ ...agent, ...patch }) : agent)),
      nodes: current.nodes.map((node) => ({
        ...node,
        agent_id: node.agent_id === selectedAgent.id && nextId ? nextId : node.agent_id
      }))
    }));
    if (nextId) setSelectedAgentId(nextId);
  }

  async function persistSelectedAgent() {
    if (!selectedAgent) return;
    try {
      const saved = await saveAgent(selectedAgent);
      refreshLibrary();
      setStatus(`Saved agent ${saved.id}`);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  async function loadAgentIntoWorkflow(agentId: string) {
    try {
      const agent = await getAgent(agentId);
      updateWorkflow((current) => ({
        ...current,
        agents: upsertAgent(current.agents, agent)
      }));
      setSelectedAgentId(agent.id);
      setStatus(`Loaded agent ${agent.id}`);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  const onNodesChange = useCallback((changes: NodeChange[]) => {
    const removedIds = changes.filter((change) => change.type === "remove").map((change) => change.id);
    setNodes((current) => applyNodeChanges(changes, current));
    if (removedIds.length > 0) {
      setWorkflow((currentWorkflow) => {
        const removed = new Set(removedIds);
        const nextWorkflow = {
          ...currentWorkflow,
          nodes: currentWorkflow.nodes.filter((node) => !removed.has(node.id)),
          edges: currentWorkflow.edges.filter((edge) => !removed.has(edge.from) && !removed.has(edge.to))
        };
        setJsonText(formatJson(nextWorkflow));
        setEdges(toFlowEdges(nextWorkflow));
        setSelectedNodeId((current) => (current && removed.has(current) ? nextWorkflow.nodes[0]?.id ?? null : current));
        return nextWorkflow;
      });
    }
  }, []);

  const onEdgesChange = useCallback(
    (changes: EdgeChange[]) => {
      setEdges((current) => {
        const nextEdges = applyEdgeChanges(changes, current);
        const specEdges = fromFlowEdges(nextEdges, workflow);
        setWorkflow((currentWorkflow) => {
          const nextWorkflow = { ...currentWorkflow, edges: specEdges };
          setJsonText(formatJson(nextWorkflow));
          return nextWorkflow;
        });
        return nextEdges;
      });
    },
    [workflow]
  );

  const onConnect = useCallback(
    (connection: Connection) => {
      setEdges((current) => {
        const nextEdges = addEdge(connection, current);
        const specEdges = fromFlowEdges(nextEdges, workflow);
        setWorkflow((currentWorkflow) => {
          const nextWorkflow = { ...currentWorkflow, edges: specEdges };
          setJsonText(formatJson(nextWorkflow));
          return nextWorkflow;
        });
        return nextEdges;
      });
    },
    [workflow]
  );

  async function runWorkflow(approvedOverride = approved) {
    setPreflightLoading(true);
    setStatus("正在运行工作流 Preflight...");
    try {
      const parsed = JSON.parse(jsonText) as WorkflowSpec;
      const scopes = linesToList(scopesText);
      const result = await validateWorkflow(parsed);
      setPendingPreflight({
        repo,
        request,
        workflow: parsed,
        approved: approvedOverride,
        scopes,
        result
      });
      if (result.status === "error") {
        setStatus("Preflight 发现错误，运行已阻止。");
      } else if (result.status === "warning") {
        setStatus("Preflight 发现警告，请确认后再启动。");
      } else {
        setStatus("Preflight 通过，请确认启动。");
      }
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    } finally {
      setPreflightLoading(false);
    }
  }

  async function startWorkflowRun(input: PendingPreflightRun) {
    setEvents([]);
    setEventCursor(0);
    setEventHasMore(false);
    setEventsLoadingMore(false);
    setActiveRunId(null);
    setPendingPreflight(null);
    setStatus(input.approved ? "Starting approved live run..." : "Starting live run...");
    try {
      const run = await startLiveRun({
        repo: input.repo,
        request: input.request,
        workflow: input.workflow,
        approved: input.approved,
        scopes: input.scopes
      });
      setActiveRunId(run.run_id);
      setSelectedRunKind("live");
      setSelectedRunDetail(null);
      setStatus(`Live run ${run.run_id}: ${run.status}`);
      subscribeToRun(run.run_id, run.events_url);
      refreshRuntimeInfo();
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  async function approveAndResumeRun(approvedValue = true, reason?: string) {
    if (!activeRunId) {
      setStatus("No blocked live run selected.");
      return;
    }
    setStatus(`${approvedValue ? "Approving" : "Rejecting"} live run ${activeRunId}...`);
    try {
      const run = await approveLiveRun(activeRunId, { approved: approvedValue, reason });
      setStatus(`Live run ${run.run_id}: ${run.status}`);
      if (approvedValue) {
        subscribeToRun(run.run_id, run.events_url);
      }
      refreshRuntimeInfo();
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  function subscribeToRun(runId: string, eventsUrl: string) {
    const source = subscribeRunEvents(
      eventsUrl,
      (event) => {
        setEvents((current) => {
          const isDuplicate = Boolean(event.id && current.some((existing) => existing.id === event.id));
          if (!isDuplicate && isTerminalRunEvent(event.type)) {
            source.close();
          }
          return upsertEvent(current, event);
        });
      },
      () => {
        setStatus(`Event stream closed for ${runId}`);
        source.close();
      }
    );
  }

  return (
    <div className="app-shell">
      <header className="topbar">
        <div>
          <div className="eyebrow">{t.app.eyebrow}</div>
          <h1>{t.app.title}</h1>
        </div>
        <div className="status">{status}</div>
      </header>

      <aside className="sidebar">
        <section className="panel">
          <div className="panel-title">{t.templates.title}</div>
          <div className="template-list">
            {workflowTemplateCards.map((template) => (
              <TemplateCard key={template.id} template={template} onUse={useTemplateCard} />
            ))}
          </div>
        </section>

        <section className="panel">
          <div className="panel-title">{t.library.title}</div>
          <button onClick={() => setCurrentWorkflow(codingWorkbenchWorkflow)}>{t.library.loadExample}</button>
          <button onClick={refreshLibrary}>{t.library.refresh}</button>
          <div className="list">
            {library.workflows.length === 0 ? (
              <div className="muted">{t.library.empty}</div>
            ) : (
              library.workflows.map((item) => (
                <button className="list-item" key={item.id} onClick={() => loadWorkflow(item.id)}>
                  <span>{item.name ?? item.id}</span>
                  <small>{t.library.nodeEdgeCount(item.nodes, item.edges)}</small>
                </button>
              ))
            )}
          </div>
        </section>

        <section className="panel">
          <div className="panel-title">{t.run.title}</div>
          <label>
            {t.run.repo}
            <input value={repo} onChange={(event) => setRepo(event.target.value)} />
          </label>
          <label>
            {t.run.scopes}
            <textarea
              placeholder={t.run.scopesPlaceholder}
              value={scopesText}
              onChange={(event) => setScopesText(event.target.value)}
              rows={3}
            />
          </label>
          <label>
            {t.run.request}
            <textarea value={request} onChange={(event) => setRequest(event.target.value)} rows={4} />
          </label>
          <label className="checkbox-row">
            <input type="checkbox" checked={approved} onChange={(event) => setApproved(event.target.checked)} />
            {t.run.preApprove}
          </label>
          <button onClick={() => runWorkflow()} disabled={preflightLoading}>
            {preflightLoading ? t.preflight.running : t.run.start}
          </button>
        </section>

        <section className="panel">
          <div className="panel-title">Provider Settings</div>
          <ProviderSettingsPanel
            form={providerForm}
            settings={providerSettings}
            status={providerStatus}
            onChange={(patch) => {
              const nextProvider = patch.default_provider?.trim().toLowerCase();
              setProviderForm((current) => {
                const merged = { ...current, ...patch };
                if (nextProvider && nextProvider !== current.default_provider) {
                  merged.base_url = providerSettings?.base_urls[nextProvider] ?? "";
                  merged.api_key = "";
                }
                return merged;
              });
            }}
            onSave={persistProviderSettings}
            onRefresh={refreshProviderInfo}
            onTest={runProviderTest}
          />
        </section>

        <section className="panel">
          <div className="panel-title">{t.runtime.title}</div>
          <button onClick={refreshRuntimeInfo}>{t.runtime.refresh}</button>
          <div className="summary-grid">
            <span>{health?.status ?? t.runtime.unknown}</span>
            <span>{t.runtime.tools(health?.tools.length ?? 0)}</span>
            <span>{t.runtime.liveRuns(liveRuns.length)}</span>
            <span>{t.runtime.storedRuns(runHistory.length)}</span>
          </div>
          <div className="list compact-list">
            {liveRuns.slice(0, 5).map((run) => (
              <button className="list-item" key={run.id} onClick={() => openLiveRun(run.id)}>
                <span>{run.workflow_id}</span>
                <small>{run.status} / {run.events} events</small>
              </button>
            ))}
            {liveRuns.length === 0 && <div className="muted">{t.runtime.noLiveRuns}</div>}
          </div>
          <div className="panel-subtitle">{t.runtime.storedHistory}</div>
          <div className="list compact-list">
            {runHistory.slice(0, 5).map((run) => (
              <button className="list-item" key={run.id} onClick={() => openStoredRun(run.id)}>
                <span>{run.workflow_id}</span>
                <small>{run.status} / {run.events} events</small>
              </button>
            ))}
            {runHistory.length === 0 && <div className="muted">{t.runtime.noStoredRuns}</div>}
          </div>
        </section>
      </aside>

      <main className="workspace">
        <section className="canvas-panel">
          <div className="toolbar">
            <div>
              <strong>{workflow.name}</strong>
              <span>{workflow.id}</span>
            </div>
            <div className="button-row">
              {nodeTypes.map((type) => (
                <button key={type} onClick={() => addWorkflowNode(type)}>
                  {t.canvas.addNode(type)}
                </button>
              ))}
            </div>
          </div>
          <ReactFlow
            nodes={nodes}
            edges={edges}
            onNodesChange={onNodesChange}
            onEdgesChange={onEdgesChange}
            onConnect={onConnect}
            onNodeClick={(_, node) => {
              setSelectedNodeId(node.id);
              setSelectedEdgeId(null);
            }}
            onEdgeClick={(_, edge) => {
              setSelectedEdgeId(edge.id);
              setSelectedNodeId(null);
            }}
            fitView
          >
            <Background />
            <Controls />
            <MiniMap />
          </ReactFlow>
        </section>

        <section className="editor-panel">
          <div className="panel-title">{t.json.title}</div>
          <div className="button-row">
            <button onClick={applyJson}>{t.json.apply}</button>
            <button onClick={persistWorkflow}>{t.json.save}</button>
            <button onClick={exportWorkflow}>{t.json.export}</button>
            <label className="file-button">
              {t.json.import}
              <input
                type="file"
                accept="application/json,.json"
                onChange={(event) => importWorkflow(event.target.files?.[0] ?? null)}
              />
            </label>
          </div>
          <textarea className="json-editor" value={jsonText} onChange={(event) => setJsonText(event.target.value)} />
        </section>
      </main>

      <aside className="inspector">
        <section className="panel">
          <div className="panel-title">{t.inspector.title}</div>
          {selectedNode ? (
            <NodeInspector node={selectedNode} workflow={workflow} onChange={updateSelectedNode} />
          ) : selectedEdge ? (
            <EdgeInspector edge={selectedEdge} nodes={workflow.nodes} onChange={updateSelectedEdge} />
          ) : (
            <div className="muted">{t.inspector.empty}</div>
          )}
        </section>

        <section className="panel">
          <div className="panel-title">{t.inspector.agents}</div>
          <div className="button-row">
            <button onClick={addAgent}>{t.inspector.addAgent}</button>
            <button disabled={!selectedAgent} onClick={persistSelectedAgent}>
              {t.inspector.saveAgent}
            </button>
          </div>
          <div className="list compact-list">
            {workflow.agents.map((agent) => (
              <button
                className={`list-item ${agent.id === selectedAgentId ? "selected" : ""}`}
                key={agent.id}
                onClick={() => setSelectedAgentId(agent.id)}
              >
                <span>{agent.name ?? agent.id}</span>
                <small>{agent.role}</small>
              </button>
            ))}
            {workflow.agents.length === 0 && <div className="muted">{t.inspector.noAgents}</div>}
          </div>
          {library.agents.length > 0 && (
            <>
              <div className="panel-subtitle">{t.inspector.libraryAgents}</div>
              <div className="list compact-list">
                {library.agents.map((agent) => (
                  <button className="list-item" key={agent.id} onClick={() => loadAgentIntoWorkflow(agent.id)}>
                    <span>{agent.name ?? agent.id}</span>
                    <small>{agent.role}</small>
                  </button>
                ))}
              </div>
            </>
          )}
          {selectedAgent && <AgentInspector agent={selectedAgent} onChange={updateSelectedAgent} />}
        </section>

        <section className="panel events-panel">
          <div className="panel-title">{t.events.title}</div>
          <RunDetailCard
            detail={selectedRunDetail}
            kind={selectedRunKind}
            activeRunId={activeRunId}
            onAttach={(runId) => openLiveRun(runId, true)}
            onOpenStored={(runId) => openStoredRun(runId)}
          />
          <RunSummary events={events} onApprovalDecision={approveAndResumeRun} />
          <PatchPanel
            events={events}
            runId={selectedRunKind === "stored" ? selectedRunDetail?.id ?? null : null}
            repo={repo}
            scopes={linesToList(scopesText)}
            onStatus={setStatus}
          />
          {events.length === 0 ? (
            <div className="muted">{t.events.empty}</div>
          ) : (
            events.map((event, index) => (
              <div className="event-row" id={eventDomId(event)} key={event.id ?? `${event.type}-${index}`}>
                <div className="event-heading">
                  <strong>{event.type}</strong>
                  {event.node_id && <code>{event.node_id}</code>}
                </div>
                <span>{event.message ?? ""}</span>
                {event.type === "agent.context_packet" && (
                  <ContextPacketCard
                    event={event}
                    runId={selectedRunKind === "stored" ? selectedRunDetail?.id ?? null : null}
                  />
                )}
                {event.type === "artifact.produced" && (
                  <ArtifactCard
                    event={event}
                    runId={selectedRunKind === "stored" ? selectedRunDetail?.id ?? null : null}
                  />
                )}
                {event.type !== "agent.context_packet" &&
                  event.type !== "artifact.produced" &&
                  event.payload &&
                  Object.keys(event.payload).length > 0 && (
                  <pre>{JSON.stringify(event.payload, null, 2)}</pre>
                )}
              </div>
            ))
          )}
          {selectedRunKind === "stored" && eventHasMore && (
            <button onClick={loadMoreStoredEvents} disabled={eventsLoadingMore}>
              {eventsLoadingMore ? "Loading events..." : "Load more events"}
            </button>
          )}
        </section>
      </aside>
      {pendingPreflight && (
        <PreflightModal
          pending={pendingPreflight}
          onCancel={() => setPendingPreflight(null)}
          onConfirm={() => startWorkflowRun(pendingPreflight)}
        />
      )}
    </div>
  );
}

function PreflightModal({
  pending,
  onCancel,
  onConfirm
}: {
  pending: PendingPreflightRun;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const { result, workflow } = pending;
  const facts = preflightFacts(workflow, pending.scopes, result);
  const errors = result.issues.filter((issue) => issue.level === "error");
  const warnings = result.issues.filter((issue) => issue.level === "warning");
  const canStart = result.status !== "error";
  const providerStatuses = preflightProviderStatuses(result);

  return (
    <div className="modal-backdrop" role="presentation">
      <section className="modal-card preflight-modal" role="dialog" aria-modal="true" aria-labelledby="preflight-title">
        <div className="modal-heading">
          <div>
            <div className="eyebrow">{t.preflight.eyebrow}</div>
            <h2 id="preflight-title">{t.preflight.title}</h2>
          </div>
          <span className={`status-pill ${statusClass(result.status === "pass" ? "run.completed" : result.status === "error" ? "run.failed" : "run.blocked")}`}>
            {preflightStatusLabel(result.status)}
          </span>
        </div>

        <div className="summary-grid preflight-summary">
          <span>{t.preflight.nodes(facts.nodes, facts.edges)}</span>
          <span>{t.preflight.reachable(facts.reachableNodes)}</span>
          <span>{t.preflight.agents(facts.agents)}</span>
          <span>{t.preflight.tokenBudget(facts.tokenBudget)}</span>
          <span>{t.preflight.stepBudget(facts.maxSteps)}</span>
          <span>{t.preflight.toolBudget(facts.maxToolCalls)}</span>
        </div>

        <div className="preflight-section">
          <div className="panel-subtitle">Provider</div>
          {providerStatuses.length === 0 ? (
            <div className="muted">No provider status returned.</div>
          ) : (
            <div className="provider-status-list">
              {providerStatuses.map((provider) => (
                <div className="tool-risk-row" key={provider.provider}>
                  <span>{provider.provider}</span>
                  <span>
                    {provider.mode} · {provider.credential_source}
                    {provider.base_url ? ` · ${provider.base_url}` : ""}
                  </span>
                  <span className={`status-pill ${provider.configured ? "good" : "warn"}`}>
                    {provider.configured ? "ready" : "mock"}
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>

        <div className="preflight-section">
          <div className="panel-subtitle">{t.preflight.permissions}</div>
          <div className="summary-grid">
            <span>{t.preflight.editAgents(facts.editAgents)}</span>
            <span>{t.preflight.commandAgents(facts.commandAgents)}</span>
            <span>{t.preflight.networkAgents(facts.networkAgents)}</span>
            <span>{t.preflight.approvalAgents(facts.approvalAgents)}</span>
            <span>{t.preflight.approvalTools(facts.approvalRequiredTools)}</span>
          </div>
        </div>

        <div className="preflight-section">
          <div className="panel-subtitle">{t.preflight.scopes}</div>
          <div className="chip-row">
            {pending.scopes.length === 0 ? (
              <span className="muted">{t.preflight.noScopes}</span>
            ) : (
              pending.scopes.map((scope) => <span className="chip" key={scope}>{scope}</span>)
            )}
          </div>
        </div>

        <div className="preflight-section">
          <div className="panel-subtitle">{t.preflight.tools}</div>
          {facts.tools.length === 0 ? (
            <div className="muted">{t.preflight.noTools}</div>
          ) : (
            <div className="preflight-tool-list">
              {facts.tools.map((tool) => (
                <div className="tool-risk-row" key={`${tool.nodeId}-${tool.name}`}>
                  <span>{tool.name}</span>
                  <span>
                    {tool.nodeId}
                    {tool.permissions.length > 0 ? ` · ${tool.permissions.join(", ")}` : ""}
                    {tool.requiresApproval ? " · approval" : ""}
                  </span>
                  <span className={`status-pill ${tool.risk === "high" ? "bad" : tool.risk === "medium" ? "warn" : "good"}`}>
                    {tool.risk}
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>

        <div className="preflight-section">
          <div className="panel-subtitle">{t.preflight.issues}</div>
          {result.issues.length === 0 ? (
            <div className="muted">{t.preflight.noIssues}</div>
          ) : (
            <div className="preflight-issues">
              {result.issues.map((issue, index) => (
                <div className={`preflight-issue ${issue.level}`} key={`${issue.code}-${issue.target_id ?? index}`}>
                  <strong>{issue.level.toUpperCase()} · {issue.code}</strong>
                  <span>{issue.message}</span>
                  <small>{issue.target_type}{issue.target_id ? `: ${issue.target_id}` : ""}</small>
                </div>
              ))}
            </div>
          )}
        </div>

        {errors.length > 0 && <div className="muted">{t.preflight.errorsBlock(errors.length)}</div>}
        {warnings.length > 0 && errors.length === 0 && <div className="muted">{t.preflight.warningsConfirm(warnings.length)}</div>}

        <div className="button-row modal-actions">
          <button onClick={onCancel}>{canStart ? t.preflight.cancel : t.preflight.close}</button>
          <button onClick={onConfirm} disabled={!canStart}>{t.preflight.confirm}</button>
        </div>
      </section>
    </div>
  );
}

function preflightStatusLabel(status: string): string {
  if (status === "pass") return t.preflight.pass;
  if (status === "warning") return t.preflight.warning;
  if (status === "error") return t.preflight.error;
  return status;
}

function preflightFacts(workflow: WorkflowSpec, scopes: string[], result: PreflightResult) {
  const summary = result.summary ?? {};
  const toolNodes = workflow.nodes.filter((node) => node.type === "tool" || node.type === "mcp_tool");
  const agents = workflow.agents;
  const backendTools = objectList(summary.tools).map((tool): PreflightToolFact => {
    const displayName = typeof tool.display_name === "string" ? tool.display_name : String(tool.tool ?? "unconfigured");
    const rawTool = typeof tool.tool === "string" && tool.tool !== displayName ? ` (${tool.tool})` : "";
    return {
      nodeId: String(tool.node_id ?? "unknown"),
      name: `${displayName}${rawTool}`,
      risk: String(tool.risk_level ?? "unknown"),
      permissions: stringList(tool.permissions),
      requiresApproval: Boolean(tool.requires_approval)
    };
  });
  const permissionSummary = objectValue(summary.permission_summary);
  return {
    nodes: numberFromSummary(summary.nodes, workflow.nodes.length),
    edges: numberFromSummary(summary.edges, workflow.edges.length),
    agents: numberFromSummary(summary.agents, workflow.agents.length),
    reachableNodes: numberFromSummary(summary.reachable_nodes, 0),
    maxSteps: numberFromSummary(summary.max_steps, workflow.max_steps),
    maxToolCalls: numberFromSummary(summary.max_tool_calls, workflow.max_tool_calls),
    tokenBudget: summary.token_budget == null ? "none" : String(summary.token_budget),
    editAgents: agents.filter((agent) => agent.permissions.edit_files).length,
    commandAgents: agents.filter((agent) => agent.permissions.run_commands).length,
    networkAgents: agents.filter((agent) => agent.permissions.use_network).length,
    approvalAgents: agents.filter((agent) => agent.permissions.requires_approval).length,
    approvalRequiredTools: numberFromSummary(permissionSummary?.approval_required_tools, 0),
    scopes,
    tools: backendTools.length > 0
      ? backendTools
      : toolNodes.map((node) => ({
          nodeId: node.id,
          name: node.type === "mcp_tool" ? `MCP: ${node.tool ?? "unconfigured"}` : node.tool ?? "unconfigured",
          risk: toolRisk(node),
          permissions: [],
          requiresApproval: false
        }))
  };
}

function preflightProviderStatuses(result: PreflightResult): ProviderStatusItem[] {
  const providerStatus = objectValue(result.summary?.provider_status);
  const providers = objectList(providerStatus?.providers);
  return providers.map((provider) => ({
    provider: String(provider.provider ?? "unknown"),
    configured: Boolean(provider.configured),
    credential_configured: Boolean(provider.credential_configured),
    credential_source: String(provider.credential_source ?? "unknown"),
    base_url: typeof provider.base_url === "string" ? provider.base_url : null,
    mode: String(provider.mode ?? "unknown")
  }));
}

function numberFromSummary(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function toolRisk(node: NodeSpec): "low" | "medium" | "high" {
  if (node.type === "mcp_tool") return "high";
  if (node.tool === "apply_patch" || node.tool === "rollback_patch") return "high";
  if (node.tool === "run_check" || node.tool === "propose_patch") return "medium";
  return "low";
}

function ContextPacketCard({ event, runId }: { event: RunEvent; runId: string | null }) {
  const payload = objectValue(event.payload);
  const inlinePacket = payload?.packet;
  const packetId = typeof payload?.packet_id === "string" ? payload.packet_id : null;
  const summary = objectValue(payload?.summary);
  const [loadedPacket, setLoadedPacket] = useState<Record<string, unknown> | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  async function loadPacket() {
    if (!runId || !packetId || loading) {
      return;
    }
    setLoading(true);
    setLoadError(null);
    try {
      const detail = await getContextPacket(runId, packetId);
      setLoadedPacket(detail.packet);
    } catch (error) {
      setLoadError(error instanceof Error ? error.message : String(error));
    } finally {
      setLoading(false);
    }
  }

  const packet = inlinePacket ?? loadedPacket;
  if (!packet || typeof packet !== "object" || Array.isArray(packet)) {
    return (
      <div className="context-packet-card">
        <div className="panel-subtitle">ContextPacket</div>
        <div className="summary-grid">
          <span>packet: {packetId ?? "inline"}</span>
          <span>size: {String(payload?.size_chars ?? "unknown")}</span>
        </div>
        {summary && <pre>{JSON.stringify(summary, null, 2)}</pre>}
        {packetId && runId && (
          <button onClick={loadPacket} disabled={loading}>
            {loading ? "Loading..." : "Load full packet"}
          </button>
        )}
        {loadError && <div className="muted">{loadError}</div>}
      </div>
    );
  }

  const value = packet as Record<string, unknown>;
  const agent = objectValue(value.agent);
  const token = objectValue(value.token_estimate);
  const project = objectValue(value.project_context);
  const loop = objectValue(value.loop);
  const selectedKeys = Array.isArray(value.selected_state_keys) ? value.selected_state_keys : [];
  const tools = Array.isArray(value.allowed_tools) ? value.allowed_tools : [];

  return (
    <div className="context-packet-card">
      <div className="panel-subtitle">ContextPacket</div>
      <div className="summary-grid">
        <span>agent: {String(agent?.id ?? "unknown")}</span>
        <span>node: {String(value.node_id ?? "unknown")}</span>
        <span>tokens: {String(token?.packet ?? "unknown")}</span>
        <span>budget: {String(token?.budget ?? "none")}</span>
      </div>
      <div className="muted">Task: {String(value.task ?? "")}</div>
      <div className="muted">Repo: {String(project?.repo_root ?? "")}</div>
      {loop && (
        <div className="approval-card">
          <div className="panel-subtitle">Loop</div>
          <div className="summary-grid">
            <span>{String(loop.node_id ?? "loop")}</span>
            <span>iteration {String(loop.iteration ?? 0)}</span>
            <span>{loop["continue"] ? "continue" : "stopped"}</span>
            <span>{String(loop.break_reason ?? "no break")}</span>
          </div>
        </div>
      )}
      <div className="summary-grid">
        <span>state keys: {selectedKeys.map(String).join(", ") || "none"}</span>
        <span>tools: {tools.map(String).join(", ") || "none"}</span>
      </div>
      <details>
        <summary>查看完整上下文包</summary>
        <pre>{JSON.stringify(value, null, 2)}</pre>
      </details>
    </div>
  );
}

function ArtifactCard({ event, runId }: { event: RunEvent; runId: string | null }) {
  const payload = objectValue(event.payload);
  const artifactId = typeof payload?.artifact_id === "string" ? payload.artifact_id : null;
  const artifactType = typeof payload?.artifact_type === "string" ? payload.artifact_type : "artifact";
  const summary = objectValue(payload?.summary);
  const [loadedArtifact, setLoadedArtifact] = useState<Record<string, unknown> | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  async function loadArtifact() {
    if (!runId || !artifactId || loading) {
      return;
    }
    setLoading(true);
    setLoadError(null);
    try {
      const detail = await getArtifact(runId, artifactId);
      setLoadedArtifact(detail.artifact);
    } catch (error) {
      setLoadError(error instanceof Error ? error.message : String(error));
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="artifact-card">
      <div className="panel-subtitle">Artifact</div>
      <div className="summary-grid">
        <span>{artifactType}</span>
        <span>{artifactId ?? "unknown id"}</span>
        <span>size: {String(payload?.size_chars ?? "unknown")}</span>
      </div>
      <ArtifactPreview artifactType={artifactType} artifact={loadedArtifact ?? summary} />
      {artifactId && runId && (
        <button onClick={loadArtifact} disabled={loading}>
          {loading ? "Loading..." : "Load full artifact"}
        </button>
      )}
      {loadError && <div className="muted">{loadError}</div>}
      {loadedArtifact && (
        <details open>
          <summary>Full artifact</summary>
          <pre>{JSON.stringify(loadedArtifact, null, 2)}</pre>
        </details>
      )}
    </div>
  );
}

function ArtifactPreview({
  artifactType,
  artifact
}: {
  artifactType: string;
  artifact: Record<string, unknown> | null;
}) {
  if (!artifact) {
    return <div className="muted">No artifact summary available.</div>;
  }
  if (artifactType === "plan_artifact") {
    const targetFiles = stringList(artifact.target_files);
    const steps = stringList(artifact.implementation_steps);
    const risks = stringList(artifact.risks);
    const checks = stringList(artifact.recommended_checks ?? artifact.checks);
    return (
      <div className="artifact-specific">
        <div className="muted">{String(artifact.summary ?? "")}</div>
        <KeyValueList
          items={[
            ["Target files", targetFiles.join(", ") || "none"],
            ["Steps", steps.length > 0 ? String(steps.length) : String(artifact.steps ?? 0)],
            ["Risks", risks.length > 0 ? String(risks.length) : String(artifact.risks ?? 0)],
            ["Checks", checks.join(", ") || "none"]
          ]}
        />
        {steps.length > 0 && <InlineList title="Implementation steps" values={steps} />}
        {risks.length > 0 && <InlineList title="Risks" values={risks} />}
      </div>
    );
  }
  if (artifactType === "patch_artifact") {
    const changedFiles = stringList(artifact.changed_files);
    const risks = stringList(artifact.risks);
    const patches = Array.isArray(artifact.patches) ? artifact.patches : [];
    return (
      <div className="artifact-specific">
        <div className="muted">{String(artifact.implementation_summary ?? artifact.summary ?? "")}</div>
        <KeyValueList
          items={[
            ["Changed files", changedFiles.join(", ") || "none"],
            ["Patches", String(patches.length || artifact.patches || 0)],
            ["Risks", risks.length > 0 ? String(risks.length) : String(artifact.risks ?? 0)],
            ["Suggested check", String(artifact.suggested_check_command ?? "none")]
          ]}
        />
        {patches.length > 0 && <PatchArtifactList patches={patches} />}
        {risks.length > 0 && <InlineList title="Risks" values={risks} />}
      </div>
    );
  }
  if (artifactType === "review_artifact") {
    const evidence = stringList(artifact.evidence);
    const issues = stringList(artifact.issues);
    return (
      <div className="artifact-specific">
        <KeyValueList
          items={[
            ["Status", String(artifact.status ?? "unknown")],
            ["Risk", String(artifact.risk_level ?? "unknown")],
            ["Issues", issues.length > 0 ? String(issues.length) : String(artifact.issues ?? 0)],
            ["Action", String(artifact.recommended_action ?? "none")]
          ]}
        />
        {issues.length > 0 && <InlineList title="Issues" values={issues} />}
        {evidence.length > 0 && <InlineList title="Evidence" values={evidence} />}
      </div>
    );
  }
  return <pre>{JSON.stringify(artifact, null, 2)}</pre>;
}

function KeyValueList({ items }: { items: [string, string][] }) {
  return (
    <div className="artifact-kv">
      {items.map(([label, value]) => (
        <div key={label}>
          <span>{label}</span>
          <strong>{value}</strong>
        </div>
      ))}
    </div>
  );
}

function InlineList({ title, values }: { title: string; values: string[] }) {
  return (
    <div className="artifact-list">
      <div className="panel-subtitle">{title}</div>
      {values.map((value, index) => (
        <div key={`${title}-${index}`}>{value}</div>
      ))}
    </div>
  );
}

function PatchArtifactList({ patches }: { patches: unknown[] }) {
  return (
    <div className="artifact-list">
      <div className="panel-subtitle">Patches</div>
      {patches.slice(0, 6).map((patch, index) => {
        const value = objectValue(patch);
        const diff = value ? value.diff : null;
        return (
          <div key={`patch-${index}`}>
            <strong>{String(value?.path ?? `patch ${index + 1}`)}</strong>
            <span>{String(value?.action ?? "change")}</span>
            {typeof diff === "string" && <code>{diff.slice(0, 180)}</code>}
            {objectValue(diff) && <code>{String(objectValue(diff)?.blob_id ?? "blob reference")}</code>}
          </div>
        );
      })}
      {patches.length > 6 && <div className="muted">+ {patches.length - 6} more patches</div>}
    </div>
  );
}

function stringList(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.map((item) => (typeof item === "string" ? item : JSON.stringify(item)));
}

function objectList(value: unknown): Record<string, unknown>[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((item) => {
    const object = objectValue(item);
    return object ? [object] : [];
  });
}

async function hydrateBlobRefs(value: unknown, runId: string): Promise<unknown> {
  const object = objectValue(value);
  if (object?.blob_id && typeof object.blob_id === "string" && typeof object.size_chars === "number") {
    const blob = await getBlob(runId, object.blob_id);
    return blob.content;
  }
  if (Array.isArray(value)) {
    return Promise.all(value.map((item) => hydrateBlobRefs(item, runId)));
  }
  if (object) {
    const entries = await Promise.all(
      Object.entries(object).map(async ([key, item]) => [key, await hydrateBlobRefs(item, runId)] as const)
    );
    return Object.fromEntries(entries);
  }
  return value;
}

function objectValue(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : null;
}

function PatchPanel({
  events,
  runId,
  repo,
  scopes,
  onStatus
}: {
  events: RunEvent[];
  runId: string | null;
  repo: string;
  scopes: string[];
  onStatus: (status: string) => void;
}) {
  const [loadedToolResults, setLoadedToolResults] = useState<Record<string, Record<string, unknown>>>({});
  const [toolResultErrors, setToolResultErrors] = useState<Record<string, string>>({});
  const [rollbackResult, setRollbackResult] = useState<Record<string, unknown> | null>(null);
  const [rollbackLoading, setRollbackLoading] = useState(false);
  const toolResultIds = useMemo(() => storedToolResultIds(events), [events]);
  const toolResultKey = toolResultIds.join("|");

  useEffect(() => {
    if (!runId) {
      setLoadedToolResults((current) => (Object.keys(current).length > 0 ? {} : current));
      setToolResultErrors((current) => (Object.keys(current).length > 0 ? {} : current));
      return;
    }
    let cancelled = false;
    const missing = toolResultIds.filter((id) => !loadedToolResults[id] && !toolResultErrors[id]);
    for (const id of missing) {
      getToolResult(runId, id)
        .then(async (detail) => {
          if (cancelled) return;
          const hydrated = await hydrateBlobRefs(detail.result, runId);
          if (cancelled) return;
          setLoadedToolResults((current) => ({ ...current, [id]: hydrated as Record<string, unknown> }));
        })
        .catch((error) => {
          if (cancelled) return;
          setToolResultErrors((current) => ({
            ...current,
            [id]: error instanceof Error ? error.message : String(error)
          }));
        });
    }
    return () => {
      cancelled = true;
    };
  }, [runId, toolResultKey, loadedToolResults, toolResultErrors, toolResultIds]);

  const patch = latestToolResult(events, "propose_patch", loadedToolResults) ?? latestToolResult(events, "dry_patch", loadedToolResults);
  const apply = latestToolResult(events, "apply_patch", loadedToolResults);
  const check = latestToolResult(events, "check", loadedToolResults);
  const files = Array.isArray(patch?.files) ? patch.files : [];
  const snapshotId = typeof apply?.snapshot_id === "string" ? apply.snapshot_id : null;
  const applyErrors = Array.isArray(apply?.errors) ? apply.errors : [];
  const isLoadingToolResult = runId ? toolResultIds.some((id) => !loadedToolResults[id] && !toolResultErrors[id]) : false;
  const toolResultErrorMessages = Object.values(toolResultErrors);
  const relatedObjects = useMemo(() => relatedPatchObjects(events), [events]);

  async function rollback() {
    if (!snapshotId) return;
    onStatus(`Rolling back snapshot ${snapshotId}...`);
    setRollbackLoading(true);
    setRollbackResult(null);
    try {
      const result = await rollbackPatch({ repo, snapshot_id: snapshotId, scopes });
      setRollbackResult(result.rollback);
      onStatus(String(result.rollback.message ?? `Rolled back ${snapshotId}`));
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setRollbackResult({ status: "failed", message });
      onStatus(message);
    } finally {
      setRollbackLoading(false);
    }
  }

  if (!patch && !apply && !check) return null;

  return (
    <div className="patch-panel">
      {(relatedObjects.artifacts.length > 0 || relatedObjects.contexts.length > 0) && (
        <div>
          <div className="panel-subtitle">Related Run Objects</div>
          <div className="object-links">
            {relatedObjects.artifacts.map((item) => (
              <a href={`#${eventDomId(item.event)}`} key={item.event.id}>
                {item.type}: {item.id.slice(0, 12)}
              </a>
            ))}
            {relatedObjects.contexts.map((item) => (
              <a href={`#${eventDomId(item.event)}`} key={item.event.id}>
                context: {item.id.slice(0, 12)}
              </a>
            ))}
          </div>
        </div>
      )}
      {patch && (
        <div>
          <div className="panel-subtitle">Patch Preview</div>
          {files.length === 0 && isLoadingToolResult ? (
            <div className="muted">Loading full tool result...</div>
          ) : files.length === 0 ? (
            <div className="muted">No file changes proposed.</div>
          ) : (
            files.map((file, index) => {
              const item = file as Record<string, unknown>;
              return (
                <div className="diff-block" key={`${String(item.path)}-${index}`}>
                  <div className="event-heading">
                    <strong>{String(item.path ?? "unknown")}</strong>
                    <code>{String(item.action ?? "update")}</code>
                  </div>
                  <pre>{String(item.diff ?? "")}</pre>
                </div>
              );
            })
          )}
        </div>
      )}
      {toolResultErrorMessages.length > 0 && (
        <div className="muted">Tool result load failed: {toolResultErrorMessages.join("; ")}</div>
      )}
      {apply && (
        <div>
          <div className="panel-subtitle">Patch Apply</div>
          <div className="summary-grid">
            <span>{String(apply.status ?? "unknown")}</span>
            {snapshotId && <span>snapshot {snapshotId.slice(0, 8)}</span>}
          </div>
          {typeof apply.message !== "undefined" && <div className="muted">{String(apply.message)}</div>}
          {applyErrors.length > 0 && (
            <div className="patch-errors">
              {applyErrors.map((error, index) => {
                const item = error as Record<string, unknown>;
                return (
                  <div className="patch-error" key={`${String(item.path ?? "unknown")}-${index}`}>
                    <strong>{String(item.path ?? "unknown")}</strong>
                    <code>{String(item.code ?? "error")}</code>
                    <span>{String(item.message ?? "Patch apply rejected.")}</span>
                  </div>
                );
              })}
            </div>
          )}
          {snapshotId && (
            <button onClick={rollback} disabled={rollbackLoading}>
              {rollbackLoading ? "Rolling back..." : "Rollback snapshot"}
            </button>
          )}
          {rollbackResult && (
            <div className="rollback-status">
              <div className="panel-subtitle">Rollback Status</div>
              <div className="summary-grid">
                <span>{String(rollbackResult.status ?? "unknown")}</span>
                <span>{String(rollbackResult.snapshot_id ?? snapshotId ?? "no snapshot")}</span>
                <span>{Array.isArray(rollbackResult.restored) ? `${rollbackResult.restored.length} files` : "files unknown"}</span>
              </div>
              {typeof rollbackResult.message !== "undefined" && <div className="muted">{String(rollbackResult.message)}</div>}
            </div>
          )}
        </div>
      )}
      {check && (
        <div>
          <div className="panel-subtitle">Check Result</div>
          <div className="summary-grid">
            <span>{check.passed ? "passed" : "not passed"}</span>
            {typeof check.returncode === "number" && <span>exit {check.returncode}</span>}
          </div>
          {typeof check.output !== "undefined" && <pre>{String(check.output)}</pre>}
        </div>
      )}
    </div>
  );
}

function TemplateCard({
  template,
  onUse
}: {
  template: WorkflowTemplateCard;
  onUse: (template: WorkflowTemplateCard) => void;
}) {
  const isDefaultCoding = template.id === "default-coding";
  const name = isDefaultCoding ? t.templates.defaultCodingName : t.templates.blankName;
  const purpose = isDefaultCoding ? t.templates.defaultCodingPurpose : t.templates.blankPurpose;
  const approvals =
    template.approvals === "requiredApprovals" ? t.templates.requiredApprovals : "无强制审批";
  const modelRequirement =
    template.modelRequirement === "optionalModel" ? t.templates.optionalModel : template.modelRequirement;
  const knowledgeRequirement =
    template.knowledgeRequirement === "projectKnowledge"
      ? t.templates.projectKnowledge
      : template.knowledgeRequirement;
  const risk = template.risk === "mediumRisk" ? t.templates.mediumRisk : t.templates.lowRisk;

  return (
    <article className="template-card">
      <div className="template-heading">
        <strong>{name}</strong>
        <span>{template.workflow.version}</span>
      </div>
      <p>{purpose}</p>
      <div className="template-meta">
        <span>
          {t.templates.agents}: {template.agentCount}
        </span>
        <span>
          {t.templates.tools}: {template.tools.length > 0 ? template.tools.join(", ") : "无"}
        </span>
        <span>
          {t.templates.approvals}: {approvals}
        </span>
        <span>
          {t.templates.model}: {modelRequirement}
        </span>
        <span>
          {t.templates.knowledge}: {knowledgeRequirement}
        </span>
        <span>
          {t.templates.risk}: {risk}
        </span>
      </div>
      <button onClick={() => onUse(template)}>{t.templates.useTemplate}</button>
    </article>
  );
}

function ProviderSettingsPanel({
  form,
  settings,
  status,
  onChange,
  onSave,
  onRefresh,
  onTest
}: {
  form: ProviderFormState;
  settings: ProviderSettings | null;
  status: ProviderStatus | null;
  onChange: (patch: Partial<ProviderFormState>) => void;
  onSave: () => void;
  onRefresh: () => void;
  onTest: () => void;
}) {
  const provider = form.default_provider.trim().toLowerCase() || "openai";
  const currentStatus =
    status?.providers.find((item) => item.provider === provider) ??
    (status?.default_status.provider === provider ? status.default_status : null);
  const keyState = settings?.api_keys[provider];

  return (
    <div className="form-stack">
      <label>
        Provider
        <select value={form.default_provider} onChange={(event) => onChange({ default_provider: event.target.value })}>
          {["openai", "deepseek", "openai-compatible", "qwen", "moonshot", "ollama"].map((providerName) => (
            <option key={providerName} value={providerName}>
              {providerName}
            </option>
          ))}
        </select>
      </label>
      <label>
        Model
        <input value={form.default_model} onChange={(event) => onChange({ default_model: event.target.value })} />
      </label>
      <label>
        Base URL
        <input
          placeholder="Provider default"
          value={form.base_url}
          onChange={(event) => onChange({ base_url: event.target.value })}
        />
      </label>
      <label>
        API Key
        <input
          type="password"
          placeholder={keyState?.configured ? `${keyState.source}: configured` : "Leave blank to keep current value"}
          value={form.api_key}
          onChange={(event) => onChange({ api_key: event.target.value })}
        />
      </label>
      <label className="checkbox-row">
        <input
          type="checkbox"
          checked={form.mock_mode}
          onChange={(event) => onChange({ mock_mode: event.target.checked })}
        />
        Use mock output when credentials are missing
      </label>
      {currentStatus && (
        <div className="summary-grid provider-summary">
          <span>{currentStatus.mode}</span>
          <span>{currentStatus.credential_source}</span>
          <span>{currentStatus.configured ? "configured" : "missing"}</span>
          <span>{currentStatus.base_url ?? "default URL"}</span>
        </div>
      )}
      <div className="button-row">
        <button onClick={onSave}>Save</button>
        <button onClick={onTest}>Test</button>
        <button onClick={onRefresh}>Refresh</button>
      </div>
    </div>
  );
}

function relatedPatchObjects(events: RunEvent[]) {
  const artifacts = events
    .filter((event) => event.type === "artifact.produced")
    .map((event) => {
      const type = String(event.payload?.artifact_type ?? "artifact");
      const id = String(event.payload?.artifact_id ?? event.id ?? "artifact");
      return { event, type, id };
    })
    .filter((item) => item.type === "patch_artifact" || item.type === "plan_artifact")
    .slice(-4);
  const contexts = events
    .filter((event) => event.type === "agent.context_packet")
    .map((event) => {
      const id = String(event.payload?.packet_id ?? event.id ?? "context");
      return { event, id };
    })
    .slice(-4);
  return { artifacts, contexts };
}

function eventDomId(event: RunEvent): string {
  return `event-${String(event.id ?? event.type).replace(/[^a-zA-Z0-9_-]/g, "-")}`;
}

function RunDetailCard({
  detail,
  kind,
  activeRunId,
  onAttach,
  onOpenStored
}: {
  detail: StoredRunDetail | LiveRunDetail | null;
  kind: "live" | "stored" | null;
  activeRunId: string | null;
  onAttach: (runId: string) => void;
  onOpenStored: (runId: string) => void;
}) {
  if (!detail || !kind) return null;
  const result = "result" in detail ? detail.result : null;
  const status = kind === "stored" ? result?.status : (detail as LiveRunDetail).status;
  const events = kind === "stored" ? result?.events.length ?? 0 : (detail as LiveRunDetail).events.length;
  const liveDetail = kind === "live" ? (detail as LiveRunDetail) : null;
  const canAttach = liveDetail?.status === "queued" || liveDetail?.status === "running" || liveDetail?.status === "blocked";

  return (
    <div className="run-detail-card">
      <div className="event-heading">
        <strong>{kind === "live" ? "Live run detail" : "Stored run detail"}</strong>
        <code>{detail.id}</code>
      </div>
      <div className="summary-grid">
        <span>{status ?? "unknown"}</span>
        <span>{events} events</span>
        {result && <span>{result.agent_calls} agent calls</span>}
        {result && <span>{result.tool_calls} tool calls</span>}
        {result && <span>{result.estimated_tokens_used} est. tokens</span>}
        {result?.blocked_node_id && <span>blocked at {result.blocked_node_id}</span>}
      </div>
      <div className="muted">Repo: {detail.repo_root}</div>
      <div className="muted">Request: {detail.request}</div>
      {liveDetail?.stored_run_id && (
        <button onClick={() => onOpenStored(liveDetail.stored_run_id as string)}>
          Open stored result
        </button>
      )}
      {canAttach && (
        <button disabled={activeRunId === detail.id && liveDetail?.status === "blocked"} onClick={() => onAttach(detail.id)}>
          {liveDetail?.status === "blocked" ? "Use this blocked run for approval" : "Reattach event stream"}
        </button>
      )}
      {liveDetail?.error && <div className="muted">Error: {liveDetail.error}</div>}
    </div>
  );
}

function latestToolResult(
  events: RunEvent[],
  nodeId: string,
  loadedToolResults: Record<string, Record<string, unknown>> = {}
): Record<string, unknown> | null {
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const event = events[index];
    if (event.type === "tool.result" && event.node_id === nodeId) {
      const result = event.payload?.result;
      if (result && typeof result === "object" && !Array.isArray(result)) return result as Record<string, unknown>;
      const toolResultId = typeof event.payload?.tool_result_id === "string" ? event.payload.tool_result_id : null;
      if (toolResultId && loadedToolResults[toolResultId]) return loadedToolResults[toolResultId];
      const resultStatus = event.payload?.result_status;
      if (typeof resultStatus === "string") {
        return {
          status: resultStatus,
          summary: event.payload?.result_summary,
          keys: event.payload?.result_keys,
          tool_result_id: toolResultId
        };
      }
      continue;
    }
    if (event.type !== "node.completed" || event.node_id !== nodeId) continue;
    const result = event.payload?.result;
    if (result && typeof result === "object" && !Array.isArray(result)) return result as Record<string, unknown>;
    const resultStatus = event.payload?.result_status;
    if (typeof resultStatus === "string") {
      return {
        status: resultStatus,
        summary: event.payload?.result_summary,
        keys: event.payload?.result_keys
      };
    }
  }
  return null;
}

function storedToolResultIds(events: RunEvent[]): string[] {
  const ids = new Set<string>();
  for (const event of events) {
    if (event.type !== "tool.result") continue;
    const toolResultId = typeof event.payload?.tool_result_id === "string" ? event.payload.tool_result_id : null;
    if (toolResultId && !event.payload?.result) ids.add(toolResultId);
  }
  return [...ids];
}

function RunSummary({
  events,
  onApprovalDecision
}: {
  events: RunEvent[];
  onApprovalDecision: (approved: boolean, reason?: string) => void;
}) {
  const [reason, setReason] = useState("");
  const latest = events.at(-1);
  const agentCalls = events.filter((event) => event.type === "agent.called").length;
  const toolCalls = events.filter((event) => event.type === "tool.called").length;
  const selectedEdges = events.filter((event) => event.type === "edge.selected").length;
  const needsApproval = events.some((event) => event.type === "approval.required");
  const approvalRequests = events.filter((event) => event.type === "approval.required");
  const approvalRecords = events.filter((event) => event.type === "approval.recorded");
  const latestApproval = approvalRequests.at(-1);
  const isBlocked = latest?.type === "run.blocked";

  if (events.length === 0) return null;

  return (
    <div className="run-summary">
      <div>
        <span className={`status-pill ${statusClass(latest?.type)}`}>{latest?.type ?? "running"}</span>
      </div>
      <div className="summary-grid">
        <span>{events.length} events</span>
        <span>{agentCalls} agent calls</span>
        <span>{toolCalls} tool calls</span>
        <span>{selectedEdges} edges</span>
      </div>
      {needsApproval && isBlocked && (
        <div className="approval-actions">
          <input placeholder="Optional approval/rejection reason" value={reason} onChange={(event) => setReason(event.target.value)} />
          <div className="button-row">
            <button onClick={() => onApprovalDecision(true, reason || undefined)}>Approve and resume</button>
            <button onClick={() => onApprovalDecision(false, reason || "Rejected by local user")}>Reject</button>
          </div>
        </div>
      )}
      {latestApproval && isBlocked && (
        <div className="approval-card">
          <div className="panel-subtitle">Pending Approval</div>
          <div className="summary-grid">
            <span>{String(latestApproval.payload?.approval_type ?? "human_gate")}</span>
            <span>{latestApproval.node_id ?? "unknown node"}</span>
          </div>
          {typeof latestApproval.payload?.command !== "undefined" && <pre>{String(latestApproval.payload.command)}</pre>}
          {typeof latestApproval.payload?.reason !== "undefined" && <div className="muted">{String(latestApproval.payload.reason)}</div>}
        </div>
      )}
      {approvalRecords.length > 0 && (
        <div className="approval-card">
          <div className="panel-subtitle">Approval Audit</div>
          {approvalRecords.map((event) => (
            <div className="approval-record" key={event.id}>
              <span>{String(event.payload?.approval_type ?? "approval")}</span>
              <span>{event.payload?.approved ? "approved" : "rejected"}</span>
              <span>{String(event.payload?.node_id ?? event.node_id ?? "unknown node")}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function upsertEvent(events: RunEvent[], next: RunEvent): RunEvent[] {
  if (!next.id) return [...events, next];
  return events.some((event) => event.id === next.id) ? events : [...events, next];
}

function mergeEvents(events: RunEvent[], incoming: RunEvent[]): RunEvent[] {
  return incoming.reduce((current, event) => upsertEvent(current, event), events);
}

function isTerminalRunEvent(type: string): boolean {
  return type === "run.completed" || type === "run.failed" || type === "run.blocked";
}

function statusClass(type: string | undefined): string {
  if (type === "run.completed") return "good";
  if (type === "run.failed") return "bad";
  if (type === "run.blocked" || type === "approval.required") return "warn";
  return "";
}

function NodeInspector({
  node,
  workflow,
  onChange
}: {
  node: NodeSpec;
  workflow: WorkflowSpec;
  onChange: (patch: Partial<NodeSpec>) => void;
}) {
  return (
    <div className="form-stack">
      <label>
        {t.forms.id}
        <input value={node.id} onChange={(event) => onChange({ id: event.target.value })} />
      </label>
      <label>
        {t.forms.type}
        <select value={node.type} onChange={(event) => onChange({ type: event.target.value as NodeType })}>
          {nodeTypes.map((type) => (
            <option key={type} value={type}>
              {nodeTypeLabels[type]} ({type})
            </option>
          ))}
        </select>
        <span className="field-help">{nodeTypeDescriptions[node.type]}</span>
      </label>
      {node.type === "agent" && (
        <label>
          {t.forms.agent}
          <select value={node.agent_id ?? ""} onChange={(event) => onChange({ agent_id: event.target.value })}>
            <option value="">{t.forms.selectAgent}</option>
            {workflow.agents.map((agent) => (
              <option key={agent.id} value={agent.id}>
                {agent.name ?? agent.id}
              </option>
            ))}
          </select>
        </label>
      )}
      {(node.type === "tool" || node.type === "mcp_tool") && (
        <label>
          {node.type === "mcp_tool" ? t.forms.mcpToolName : t.forms.tool}
          <input value={node.tool ?? ""} onChange={(event) => onChange({ tool: event.target.value })} />
        </label>
      )}
      {(node.type === "tool" || node.type === "mcp_tool") && (
        <label>
          {t.forms.inputJson}
          <textarea
            defaultValue={formatJson(node.input ?? {})}
            onBlur={(event) => {
              try {
                onChange({ input: JSON.parse(event.target.value) as Record<string, unknown> });
              } catch {
                event.currentTarget.value = formatJson(node.input ?? {});
              }
            }}
            rows={5}
          />
        </label>
      )}
      {node.type === "condition" && (
        <label>
          {t.forms.condition}
          <input value={node.condition ?? ""} onChange={(event) => onChange({ condition: event.target.value })} />
        </label>
      )}
      {node.type === "loop" && (
        <>
          <label>
            {t.forms.loopMode}
            <select value={node.loop_mode ?? "retry_until"} onChange={(event) => onChange({ loop_mode: event.target.value as LoopMode })}>
              {loopModes.map((mode) => (
                <option key={mode} value={mode}>
                  {mode}
                </option>
              ))}
            </select>
            <span className="field-help">
              retry_until 在条件为 true 时退出；while 在条件为 false 时退出；for_each 遍历列表输入键。连线条件通常使用
              loop_output.should_continue。
            </span>
          </label>
          {(node.loop_mode ?? "retry_until") !== "for_each" && (
            <label>
              {t.forms.condition}
              <input value={node.condition ?? ""} onChange={(event) => onChange({ condition: event.target.value })} />
            </label>
          )}
          {(node.loop_mode ?? "retry_until") === "for_each" && (
            <>
              <label>
                {t.forms.itemsKey}
                <input value={node.items_key ?? ""} onChange={(event) => onChange({ items_key: event.target.value })} />
              </label>
              <label>
                {t.forms.itemKey}
                <input value={node.item_key ?? ""} onChange={(event) => onChange({ item_key: event.target.value })} />
              </label>
            </>
          )}
          <label>
            {t.forms.maxIterations}
            <input
              type="number"
              min={1}
              max={50}
              value={node.max_iterations ?? 3}
              onChange={(event) => onChange({ max_iterations: Number(event.target.value) })}
            />
          </label>
          <label>
            {t.forms.iterationKey}
            <input value={node.iteration_key ?? ""} onChange={(event) => onChange({ iteration_key: event.target.value })} />
          </label>
          <label>
            {t.forms.collectKey}
            <input value={node.collect_key ?? ""} onChange={(event) => onChange({ collect_key: event.target.value })} />
          </label>
          <label>
            {t.forms.summaryKey}
            <input value={node.summary_key ?? ""} onChange={(event) => onChange({ summary_key: event.target.value })} />
          </label>
        </>
      )}
      {node.type === "human_gate" && (
        <label>
          {t.forms.approvalReason}
          <textarea
            value={node.approval_reason ?? ""}
            onChange={(event) => onChange({ approval_reason: event.target.value })}
            rows={3}
          />
        </label>
      )}
      <label>
        {t.forms.outputKey}
        <input value={node.output_key ?? ""} onChange={(event) => onChange({ output_key: event.target.value })} />
      </label>
    </div>
  );
}

function EdgeInspector({
  edge,
  nodes,
  onChange
}: {
  edge: EdgeSpec;
  nodes: NodeSpec[];
  onChange: (patch: Partial<EdgeSpec>) => void;
}) {
  return (
    <div className="form-stack">
      <label>
        {t.forms.from}
        <select value={edge.from} onChange={(event) => onChange({ from: event.target.value })}>
          {nodes.map((node) => (
            <option key={node.id} value={node.id}>
              {node.id}
            </option>
          ))}
        </select>
      </label>
      <label>
        {t.forms.to}
        <select value={edge.to} onChange={(event) => onChange({ to: event.target.value })}>
          {nodes.map((node) => (
            <option key={node.id} value={node.id}>
              {node.id}
            </option>
          ))}
        </select>
      </label>
      <label>
        {t.forms.condition}
        <input
          placeholder="可选，例如 approval.approved == True"
          value={edge.when ?? ""}
          onChange={(event) => onChange({ when: event.target.value })}
        />
      </label>
      <label>
        {t.forms.priority}
        <input
          type="number"
          value={edge.priority ?? 0}
          onChange={(event) => onChange({ priority: Number(event.target.value) })}
        />
      </label>
      <label>
        {t.forms.maxTraversals}
        <input
          type="number"
          min={1}
          value={edge.max_traversals ?? ""}
          onChange={(event) =>
            onChange({ max_traversals: event.target.value ? Number(event.target.value) : null })
          }
        />
      </label>
    </div>
  );
}

function AgentInspector({
  agent,
  onChange
}: {
  agent: AgentSpec;
  onChange: (patch: Partial<AgentSpec>) => void;
}) {
  return (
    <div className="form-stack agent-editor">
      <label>
        {t.forms.id}
        <input value={agent.id} onChange={(event) => onChange({ id: event.target.value })} />
      </label>
      <label>
        {t.forms.name}
        <input value={agent.name ?? ""} onChange={(event) => onChange({ name: event.target.value })} />
      </label>
      <label>
        {t.forms.role}
        <input value={agent.role} onChange={(event) => onChange({ role: event.target.value })} />
      </label>
      <label>
        {t.forms.goal}
        <textarea value={agent.goal} onChange={(event) => onChange({ goal: event.target.value })} rows={3} />
      </label>
      <label>
        {t.forms.instructions}
        <textarea
          value={agent.instructions}
          onChange={(event) => onChange({ instructions: event.target.value })}
          rows={5}
        />
      </label>
      <label>
        {t.forms.provider}
        <input value={agent.provider ?? ""} onChange={(event) => onChange({ provider: event.target.value })} />
      </label>
      <label>
        {t.forms.model}
        <input value={agent.model ?? ""} onChange={(event) => onChange({ model: event.target.value })} />
      </label>
      <label>
        {t.forms.tool}
        <input value={agent.tools.join(", ")} onChange={(event) => onChange({ tools: csvToList(event.target.value) })} />
      </label>
      <label>
        {t.forms.outputKey}
        <input value={agent.output_key ?? ""} onChange={(event) => onChange({ output_key: event.target.value })} />
      </label>
      <div className="panel-subtitle">{t.forms.permissions}</div>
      <label className="checkbox-row">
        <input
          type="checkbox"
          checked={agent.permissions.read_files}
          onChange={(event) => onChange({ permissions: { ...agent.permissions, read_files: event.target.checked } })}
        />
        {t.forms.readFiles}
      </label>
      <label className="checkbox-row">
        <input
          type="checkbox"
          checked={agent.permissions.edit_files}
          onChange={(event) => onChange({ permissions: { ...agent.permissions, edit_files: event.target.checked } })}
        />
        {t.forms.editFiles}
      </label>
      <label className="checkbox-row">
        <input
          type="checkbox"
          checked={agent.permissions.run_commands}
          onChange={(event) => onChange({ permissions: { ...agent.permissions, run_commands: event.target.checked } })}
        />
        {t.forms.runCommands}
      </label>
      <label className="checkbox-row">
        <input
          type="checkbox"
          checked={agent.permissions.use_network}
          onChange={(event) => onChange({ permissions: { ...agent.permissions, use_network: event.target.checked } })}
        />
        {t.forms.useNetwork}
      </label>
      <label className="checkbox-row">
        <input
          type="checkbox"
          checked={agent.permissions.requires_approval}
          onChange={(event) =>
            onChange({ permissions: { ...agent.permissions, requires_approval: event.target.checked } })
          }
        />
        {t.forms.requiresApproval}
      </label>
      <div className="panel-subtitle">{t.forms.contextPolicy}</div>
      <label>
        {t.forms.inputKeys}
        <input
          value={agent.context.input_keys.join(", ")}
          onChange={(event) => onChange({ context: { ...agent.context, input_keys: csvToList(event.target.value) } })}
        />
      </label>
      <label>
        {t.forms.summaryKeys}
        <input
          value={agent.context.summary_keys.join(", ")}
          onChange={(event) => onChange({ context: { ...agent.context, summary_keys: csvToList(event.target.value) } })}
        />
      </label>
    </div>
  );
}

function toFlowNodes(workflow: WorkflowSpec): FlowNode[] {
  return workflow.nodes.map((node, index) => ({
    id: node.id,
    type: "default",
    position: { x: (index % 3) * 260, y: Math.floor(index / 3) * 150 },
    data: {
      label: nodeDisplayLabel(node, workflow)
    },
    className: `workflow-node node-${node.type}`
  }));
}

function nodeDisplayLabel(node: NodeSpec, workflow: WorkflowSpec): string {
  const typeLabel = nodeTypeLabels[node.type];
  if (node.type === "agent") {
    const agent = workflow.agents.find((candidate) => candidate.id === node.agent_id);
    return `${typeLabel}: ${agent?.name ?? node.agent_id ?? "未选择"}\n${node.id}`;
  }
  if (node.type === "tool" || node.type === "mcp_tool") {
    return `${typeLabel}: ${node.tool ?? "未配置"}\n${node.id}`;
  }
  if (node.type === "loop") {
    return `${typeLabel}: ${node.loop_mode ?? "retry_until"} ×${node.max_iterations ?? 3}\n${node.id}`;
  }
  return `${typeLabel}\n${node.id}`;
}

function toFlowEdges(workflow: WorkflowSpec): FlowEdge[] {
  return workflow.edges.map((edge, index) => ({
    id: edgeIdFromIndex(index),
    source: edge.from,
    target: edge.to,
    label: edge.when ?? undefined,
    animated: Boolean(edge.when)
  }));
}

function fromFlowEdges(flowEdges: FlowEdge[], workflow: WorkflowSpec) {
  return flowEdges
    .filter((edge) => edge.source && edge.target)
    .map((edge) => {
      const existing = workflow.edges.find((candidate) => candidate.from === edge.source && candidate.to === edge.target);
      return {
        from: edge.source,
        to: edge.target,
        when: existing?.when ?? null,
        priority: existing?.priority ?? 0,
        max_traversals: existing?.max_traversals ?? null
      };
    });
}

function uniqueNodeId(workflow: WorkflowSpec, type: NodeType): string {
  const used = new Set(workflow.nodes.map((node) => node.id));
  let index = 1;
  let candidate: string = type;
  while (used.has(candidate)) {
    candidate = `${type}_${index}`;
    index += 1;
  }
  return candidate;
}

function uniqueAgentId(workflow: WorkflowSpec): string {
  const used = new Set(workflow.agents.map((agent) => agent.id));
  let index = 1;
  let candidate = "agent";
  while (used.has(candidate)) {
    candidate = `agent_${index}`;
    index += 1;
  }
  return candidate;
}

function createDefaultAgent(id: string): AgentSpec {
  return {
    id,
    name: "New Agent",
    role: "Agent",
    goal: "Describe this agent's purpose.",
    instructions: "",
    provider: null,
    model: null,
    tools: [],
    output_key: id,
    permissions: {
      read_files: true,
      edit_files: false,
      run_commands: false,
      use_network: false,
      requires_approval: true
    },
    context: {
      input_keys: [],
      summary_keys: [],
      max_items_per_key: 20,
      max_chars_per_value: 4000,
      include_all_state: false,
      include_event_history: false,
      include_full_outputs: false
    }
  };
}

function cleanNode(node: NodeSpec): NodeSpec {
  return {
    id: node.id,
    type: node.type,
    ...(node.type === "agent" ? { agent_id: node.agent_id || "agent_id" } : {}),
    ...(node.type === "tool" ? { tool: node.tool || "project_index" } : {}),
    ...(node.type === "mcp_tool" ? { tool: node.tool || "tool_name" } : {}),
    ...(node.type === "condition" ? { condition: node.condition || "state.value == True" } : {}),
    ...(node.type === "loop"
      ? {
          loop_mode: node.loop_mode || "retry_until",
          ...(node.condition ? { condition: node.condition } : {}),
          ...(node.items_key ? { items_key: node.items_key } : {}),
          ...(node.item_key ? { item_key: node.item_key } : {}),
          ...(node.iteration_key ? { iteration_key: node.iteration_key } : {}),
          max_iterations: node.max_iterations || 3,
          ...(node.collect_key ? { collect_key: node.collect_key } : {}),
          ...(node.summary_key ? { summary_key: node.summary_key } : {})
        }
      : {}),
    ...(node.type === "human_gate" && node.approval_reason ? { approval_reason: node.approval_reason } : {}),
    ...(node.output_key ? { output_key: node.output_key } : {}),
    ...(node.input && Object.keys(node.input).length > 0 ? { input: node.input } : {})
  };
}

function cleanEdge(edge: EdgeSpec): EdgeSpec {
  return {
    from: edge.from,
    to: edge.to,
    ...(edge.when ? { when: edge.when } : {}),
    ...(edge.priority ? { priority: edge.priority } : {}),
    ...(edge.max_traversals ? { max_traversals: edge.max_traversals } : {})
  };
}

function cleanAgent(agent: AgentSpec): AgentSpec {
  return {
    ...agent,
    name: agent.name || null,
    provider: agent.provider || null,
    model: agent.model || null,
    output_key: agent.output_key || null
  };
}

function upsertAgent(agents: AgentSpec[], next: AgentSpec): AgentSpec[] {
  return agents.some((agent) => agent.id === next.id)
    ? agents.map((agent) => (agent.id === next.id ? next : agent))
    : [...agents, next];
}

function csvToList(value: string): string[] {
  return value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

function linesToList(value: string): string[] {
  return value
    .split(/\r?\n/)
    .map((item) => item.trim())
    .filter(Boolean);
}

function edgeIdFromIndex(index: number): string {
  return `edge-${index}`;
}

function edgeIndexFromId(id: string): number | null {
  const match = /^edge-(\d+)$/.exec(id);
  return match ? Number(match[1]) : null;
}

function downloadJson(filename: string, value: unknown) {
  const blob = new Blob([JSON.stringify(value, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  link.click();
  URL.revokeObjectURL(url);
}

function formatJson(value: unknown) {
  return JSON.stringify(value, null, 2);
}
