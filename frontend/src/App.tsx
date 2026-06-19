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
  getHealth,
  getAgent,
  getLibrary,
  getLiveRun,
  getLiveRuns,
  getRun,
  getRuns,
  getWorkflow,
  rollbackPatch,
  saveAgent,
  saveWorkflow,
  startLiveRun,
  subscribeRunEvents
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
  NodeSpec,
  NodeType,
  RunEvent,
  RunSummaryItem,
  StoredRunDetail,
  WorkflowSpec
} from "./types";

const nodeTypes: NodeType[] = ["start", "agent", "tool", "mcp_tool", "condition", "human_gate", "end"];
const t = zhCN;

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
  const [activeRunId, setActiveRunId] = useState<string | null>(null);
  const [selectedEdgeId, setSelectedEdgeId] = useState<string | null>(null);
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null);
  const [runHistory, setRunHistory] = useState<RunSummaryItem[]>([]);
  const [liveRuns, setLiveRuns] = useState<RunSummaryItem[]>([]);
  const [health, setHealth] = useState<HealthStatus | null>(null);
  const [selectedRunDetail, setSelectedRunDetail] = useState<StoredRunDetail | LiveRunDetail | null>(null);
  const [selectedRunKind, setSelectedRunKind] = useState<"live" | "stored" | null>(null);

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

  async function openStoredRun(runId: string) {
    setStatus(`Loading stored run ${runId}...`);
    try {
      const detail = await getRun(runId);
      setSelectedRunKind("stored");
      setSelectedRunDetail(detail);
      setActiveRunId(null);
      setEvents(detail.result.events);
      setRepo(detail.repo_root);
      setRequest(detail.request);
      setStatus(`Stored run ${runId}: ${detail.result.status}`);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
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
          ...(type === "condition" ? { condition: "state.value == True" } : {})
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
    setEvents([]);
    setActiveRunId(null);
    setStatus(approvedOverride ? "Starting approved live run..." : "Starting live run...");
    try {
      const parsed = JSON.parse(jsonText) as WorkflowSpec;
      const run = await startLiveRun({
        repo,
        request,
        workflow: parsed,
        approved: approvedOverride,
        scopes: linesToList(scopesText)
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
          <button onClick={() => runWorkflow()}>{t.run.start}</button>
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
            repo={repo}
            scopes={linesToList(scopesText)}
            onStatus={setStatus}
          />
          {events.length === 0 ? (
            <div className="muted">{t.events.empty}</div>
          ) : (
            events.map((event, index) => (
              <div className="event-row" key={`${event.type}-${index}`}>
                <div className="event-heading">
                  <strong>{event.type}</strong>
                  {event.node_id && <code>{event.node_id}</code>}
                </div>
                <span>{event.message ?? ""}</span>
                {event.payload && Object.keys(event.payload).length > 0 && (
                  <pre>{JSON.stringify(event.payload, null, 2)}</pre>
                )}
              </div>
            ))
          )}
        </section>
      </aside>
    </div>
  );
}

function PatchPanel({
  events,
  repo,
  scopes,
  onStatus
}: {
  events: RunEvent[];
  repo: string;
  scopes: string[];
  onStatus: (status: string) => void;
}) {
  const patch = latestToolResult(events, "propose_patch") ?? latestToolResult(events, "dry_patch");
  const apply = latestToolResult(events, "apply_patch");
  const check = latestToolResult(events, "check");
  const files = Array.isArray(patch?.files) ? patch.files : [];
  const snapshotId = typeof apply?.snapshot_id === "string" ? apply.snapshot_id : null;
  const applyErrors = Array.isArray(apply?.errors) ? apply.errors : [];

  async function rollback() {
    if (!snapshotId) return;
    onStatus(`Rolling back snapshot ${snapshotId}...`);
    try {
      const result = await rollbackPatch({ repo, snapshot_id: snapshotId, scopes });
      onStatus(String(result.rollback.message ?? `Rolled back ${snapshotId}`));
    } catch (error) {
      onStatus(error instanceof Error ? error.message : String(error));
    }
  }

  if (!patch && !apply && !check) return null;

  return (
    <div className="patch-panel">
      {patch && (
        <div>
          <div className="panel-subtitle">Patch Preview</div>
          {files.length === 0 ? (
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
          {snapshotId && <button onClick={rollback}>Rollback snapshot</button>}
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

function latestToolResult(events: RunEvent[], nodeId: string): Record<string, unknown> | null {
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const event = events[index];
    if (event.type !== "node.completed" || event.node_id !== nodeId) continue;
    const result = event.payload?.result;
    if (result && typeof result === "object" && !Array.isArray(result)) return result as Record<string, unknown>;
  }
  return null;
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
