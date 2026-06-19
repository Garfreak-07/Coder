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
import { nodeTypeLabel, zh } from "./i18n";
import { workflowTemplate } from "./template";
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

export function App() {
  const [library, setLibrary] = useState<LibraryIndex>({ agents: [], workflows: [] });
  const [workflow, setWorkflow] = useState<WorkflowSpec>(workflowTemplate);
  const [jsonText, setJsonText] = useState(() => formatJson(workflowTemplate));
  const [nodes, setNodes] = useState<FlowNode[]>(() => toFlowNodes(workflowTemplate));
  const [edges, setEdges] = useState<FlowEdge[]>(() => toFlowEdges(workflowTemplate));
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>("start");
  const [status, setStatus] = useState("就绪");
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
      .catch((error) => setStatus(`加载库失败：${error.message}`));
  }

  function refreshRuntimeInfo() {
    Promise.all([getRuns(), getLiveRuns(), getHealth()])
      .then(([runs, live, nextHealth]) => {
        setRunHistory(runs);
        setLiveRuns(live);
        setHealth(nextHealth);
      })
      .catch((error) => setStatus(`加载运行状态失败：${error.message}`));
  }

  async function openStoredRun(runId: string) {
    setStatus(`正在加载历史运行 ${runId}...`);
    try {
      const detail = await getRun(runId);
      setSelectedRunKind("stored");
      setSelectedRunDetail(detail);
      setActiveRunId(null);
      setEvents(detail.result.events);
      setRepo(detail.repo_root);
      setRequest(detail.request);
      setStatus(`历史运行 ${runId}：${detail.result.status}`);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  async function openLiveRun(runId: string, attach = false) {
    setStatus(`正在加载实时运行 ${runId}...`);
    try {
      const detail = await getLiveRun(runId);
      setSelectedRunKind("live");
      setSelectedRunDetail(detail);
      setActiveRunId(detail.id);
      setEvents(detail.events);
      setRepo(detail.repo_root);
      setRequest(detail.request);
      setStatus(`实时运行 ${runId}：${detail.status}`);
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

  async function loadWorkflow(workflowId: string) {
    setStatus(`正在加载 ${workflowId}...`);
    try {
      setCurrentWorkflow(await getWorkflow(workflowId));
      setStatus(`已加载 ${workflowId}`);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  function applyJson() {
    try {
      const parsed = JSON.parse(jsonText) as WorkflowSpec;
      setCurrentWorkflow(parsed);
      setStatus("JSON 已在本地应用。保存后才会持久化。");
    } catch (error) {
      setStatus(error instanceof Error ? `JSON 无效：${error.message}` : "JSON 无效");
    }
  }

  async function persistWorkflow() {
    try {
      const parsed = JSON.parse(jsonText) as WorkflowSpec;
      const saved = await saveWorkflow(parsed);
      setCurrentWorkflow(saved);
      refreshLibrary();
      setStatus(`已保存工作流 ${saved.id}`);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  function exportWorkflow() {
    try {
      const parsed = JSON.parse(jsonText) as WorkflowSpec;
      downloadJson(`${parsed.id || "workflow"}.json`, parsed);
      setStatus(`已导出 ${parsed.id || "workflow"}`);
    } catch (error) {
      setStatus(error instanceof Error ? `无法导出无效 JSON：${error.message}` : "无法导出无效 JSON");
    }
  }

  function importWorkflow(file: File | null) {
    if (!file) return;
    file
      .text()
      .then((text) => {
        const parsed = JSON.parse(text) as WorkflowSpec;
        setCurrentWorkflow(parsed);
        setStatus(`已导入 ${parsed.id}`);
      })
      .catch((error) => setStatus(error instanceof Error ? `导入失败：${error.message}` : "导入失败"));
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
      setStatus(`已保存 Agent ${saved.id}`);
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
      setStatus(`已加载 Agent ${agent.id}`);
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
    setStatus(approvedOverride ? "正在启动已预审批的实时运行..." : "正在启动实时运行...");
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
      setStatus(`实时运行 ${run.run_id}：${run.status}`);
      subscribeToRun(run.run_id, run.events_url);
      refreshRuntimeInfo();
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  async function approveAndResumeRun(approvedValue = true, reason?: string) {
    if (!activeRunId) {
      setStatus("未选择被阻塞的实时运行。");
      return;
    }
    setStatus(`${approvedValue ? "正在批准" : "正在拒绝"}实时运行 ${activeRunId}...`);
    try {
      const run = await approveLiveRun(activeRunId, { approved: approvedValue, reason });
      setStatus(`实时运行 ${run.run_id}：${run.status}`);
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
        setStatus(`运行 ${runId} 的事件流已关闭`);
        source.close();
      }
    );
  }

  return (
    <div className="app-shell">
      <header className="topbar">
        <div>
          <div className="eyebrow">{zh.app.eyebrow}</div>
          <h1>{zh.app.title}</h1>
        </div>
        <div className="status">{status}</div>
      </header>

      <aside className="sidebar">
        <section className="panel">
          <div className="panel-title">{zh.labels.workflowLibrary}</div>
          <TemplateCard onUse={() => setCurrentWorkflow({ ...codingWorkbenchWorkflow, id: `coding-workflow-${Date.now()}` })} />
          <button onClick={() => setCurrentWorkflow({ ...workflowTemplate, id: `workflow-${Date.now()}` })}>
            {zh.actions.newBlank}
          </button>
          <button onClick={refreshLibrary}>{zh.actions.refresh}</button>
          <div className="list">
            {library.workflows.length === 0 ? (
              <div className="muted">{zh.helper.noSavedWorkflows}</div>
            ) : (
              library.workflows.map((item) => (
                <button className="list-item" key={item.id} onClick={() => loadWorkflow(item.id)}>
                  <span>{item.name ?? item.id}</span>
                  <small>
                    {item.nodes} 个节点 / {item.edges} 条连接
                  </small>
                </button>
              ))
            )}
          </div>
        </section>

        <section className="panel">
          <div className="panel-title">运行</div>
          <label>
            {zh.labels.repo}
            <input value={repo} onChange={(event) => setRepo(event.target.value)} />
          </label>
          <label>
            {zh.labels.scopes}
            <textarea
              placeholder={zh.helper.scopesPlaceholder}
              value={scopesText}
              onChange={(event) => setScopesText(event.target.value)}
              rows={3}
            />
          </label>
          <label>
            {zh.labels.request}
            <textarea value={request} onChange={(event) => setRequest(event.target.value)} rows={4} />
          </label>
          <label className="checkbox-row">
            <input type="checkbox" checked={approved} onChange={(event) => setApproved(event.target.checked)} />
            预先批准审批门
          </label>
          <button onClick={() => runWorkflow()}>{zh.actions.startRun}</button>
        </section>

        <section className="panel">
          <div className="panel-title">{zh.labels.runtime}</div>
          <button onClick={refreshRuntimeInfo}>{zh.actions.refreshRuntime}</button>
          <div className="summary-grid">
            <span>{health?.status ?? "unknown"}</span>
            <span>{health?.tools.length ?? 0} 个工具</span>
            <span>{liveRuns.length} 个实时运行</span>
            <span>{runHistory.length} 个历史运行</span>
          </div>
          <div className="list compact-list">
            {liveRuns.slice(0, 5).map((run) => (
              <button className="list-item" key={run.id} onClick={() => openLiveRun(run.id)}>
                <span>{run.workflow_id}</span>
                <small>{run.status} / {run.events} 个事件</small>
              </button>
            ))}
            {liveRuns.length === 0 && <div className="muted">{zh.helper.noLiveRuns}</div>}
          </div>
          <div className="panel-subtitle">{zh.labels.storedRunHistory}</div>
          <div className="list compact-list">
            {runHistory.slice(0, 5).map((run) => (
              <button className="list-item" key={run.id} onClick={() => openStoredRun(run.id)}>
                <span>{run.workflow_id}</span>
                <small>{run.status} / {run.events} 个事件</small>
              </button>
            ))}
            {runHistory.length === 0 && <div className="muted">暂无历史运行。</div>}
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
                  + {nodeTypeLabel(type)}
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
          <div className="panel-title">{zh.labels.workflowAdvanced}</div>
          <div className="muted">{zh.helper.advancedJson}</div>
          <div className="button-row">
            <button onClick={applyJson}>{zh.actions.applyJson}</button>
            <button onClick={persistWorkflow}>{zh.actions.save}</button>
            <button onClick={exportWorkflow}>{zh.actions.export}</button>
            <label className="file-button">
              {zh.actions.import}
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
          <div className="panel-title">{zh.labels.inspector}</div>
          {selectedNode ? (
            <NodeInspector node={selectedNode} workflow={workflow} onChange={updateSelectedNode} />
          ) : selectedEdge ? (
            <EdgeInspector edge={selectedEdge} nodes={workflow.nodes} onChange={updateSelectedEdge} />
          ) : (
            <div className="muted">{zh.helper.noSelection}</div>
          )}
        </section>

        <section className="panel">
          <div className="panel-title">{zh.labels.agents}</div>
          <div className="button-row">
            <button onClick={addAgent}>+ agent</button>
            <button disabled={!selectedAgent} onClick={persistSelectedAgent}>
              {zh.actions.saveAgent}
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
            {workflow.agents.length === 0 && <div className="muted">{zh.helper.noAgents}</div>}
          </div>
          {library.agents.length > 0 && (
            <>
              <div className="panel-subtitle">{zh.labels.libraryAgents}</div>
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
          <div className="panel-title">{zh.labels.eventLog}</div>
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
            <div className="muted">{zh.helper.noEvents}</div>
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

function TemplateCard({ onUse }: { onUse: () => void }) {
  return (
    <div className="template-card">
      <div className="event-heading">
        <strong>{zh.template.name}</strong>
        <code>v0.2</code>
      </div>
      <p>{zh.template.purpose}</p>
      <dl>
        <div>
          <dt>{zh.labels.agents}</dt>
          <dd>{zh.template.agents}</dd>
        </div>
        <div>
          <dt>{zh.labels.tools}</dt>
          <dd>{zh.template.tools}</dd>
        </div>
        <div>
          <dt>审批</dt>
          <dd>{zh.template.approvals}</dd>
        </div>
        <div>
          <dt>模型要求</dt>
          <dd>{zh.template.model}</dd>
        </div>
        <div>
          <dt>知识来源</dt>
          <dd>{zh.template.knowledge}</dd>
        </div>
        <div>
          <dt>风险级别</dt>
          <dd>{zh.template.risk}</dd>
        </div>
      </dl>
      <button onClick={onUse}>{zh.actions.loadTemplate}</button>
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
    onStatus(`正在回滚快照 ${snapshotId}...`);
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
          <div className="panel-subtitle">{zh.labels.patchPreview}</div>
          {files.length === 0 ? (
            <div className="muted">{zh.helper.noFileChanges}</div>
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
          <div className="panel-subtitle">{zh.labels.patchApply}</div>
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
                    <span>{String(item.message ?? "补丁应用已拒绝。")}</span>
                  </div>
                );
              })}
            </div>
          )}
          {snapshotId && <button onClick={rollback}>{zh.actions.rollback}</button>}
        </div>
      )}
      {check && (
        <div>
          <div className="panel-subtitle">{zh.labels.checkResult}</div>
          <div className="summary-grid">
            <span>{check.passed ? "通过" : "未通过"}</span>
            {typeof check.returncode === "number" && <span>exit {check.returncode}</span>}
          </div>
          {typeof check.output !== "undefined" && <pre>{String(check.output)}</pre>}
        </div>
      )}
    </div>
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
        <strong>{kind === "live" ? zh.labels.liveRunDetail : zh.labels.storedRunDetail}</strong>
        <code>{detail.id}</code>
      </div>
      <div className="summary-grid">
        <span>{status ?? "unknown"}</span>
        <span>{events} 个事件</span>
        {result && <span>{result.agent_calls} 次 Agent 调用</span>}
        {result && <span>{result.tool_calls} 次工具调用</span>}
        {result && <span>{result.estimated_tokens_used} 估算 token</span>}
        {result?.blocked_node_id && <span>阻塞于 {result.blocked_node_id}</span>}
      </div>
      <div className="muted">{zh.labels.repo}: {detail.repo_root}</div>
      <div className="muted">{zh.labels.request}: {detail.request}</div>
      {liveDetail?.stored_run_id && (
        <button onClick={() => onOpenStored(liveDetail.stored_run_id as string)}>
          打开历史结果
        </button>
      )}
      {canAttach && (
        <button disabled={activeRunId === detail.id && liveDetail?.status === "blocked"} onClick={() => onAttach(detail.id)}>
          {liveDetail?.status === "blocked" ? "选择此阻塞运行进行审批" : "重新连接事件流"}
        </button>
      )}
      {liveDetail?.error && <div className="muted">错误：{liveDetail.error}</div>}
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
        <span>{events.length} 个事件</span>
        <span>{agentCalls} 次 Agent 调用</span>
        <span>{toolCalls} 次工具调用</span>
        <span>{selectedEdges} 条连接</span>
      </div>
      {needsApproval && isBlocked && (
        <div className="approval-actions">
          <input placeholder={zh.helper.optionalApprovalReason} value={reason} onChange={(event) => setReason(event.target.value)} />
          <div className="button-row">
            <button onClick={() => onApprovalDecision(true, reason || undefined)}>{zh.actions.approve}</button>
            <button onClick={() => onApprovalDecision(false, reason || "Rejected by local user")}>{zh.actions.reject}</button>
          </div>
        </div>
      )}
      {latestApproval && isBlocked && (
        <div className="approval-card">
          <div className="panel-subtitle">{zh.labels.pendingApproval}</div>
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
          <div className="panel-subtitle">{zh.labels.approvalAudit}</div>
          {approvalRecords.map((event) => (
            <div className="approval-record" key={event.id}>
              <span>{String(event.payload?.approval_type ?? "approval")}</span>
              <span>{event.payload?.approved ? "已批准" : "已拒绝"}</span>
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
        {zh.labels.id}
        <input value={node.id} onChange={(event) => onChange({ id: event.target.value })} />
      </label>
      <label>
        {zh.labels.nodeType}
        <select value={node.type} onChange={(event) => onChange({ type: event.target.value as NodeType })}>
          {nodeTypes.map((type) => (
            <option key={type} value={type}>{nodeTypeLabel(type)}</option>
          ))}
        </select>
      </label>
      {node.type === "agent" && (
        <label>
          {zh.labels.agent}
          <select value={node.agent_id ?? ""} onChange={(event) => onChange({ agent_id: event.target.value })}>
            <option value="">{zh.helper.selectAgent}</option>
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
          {node.type === "mcp_tool" ? "MCP 工具名" : zh.labels.tools}
          <input value={node.tool ?? ""} onChange={(event) => onChange({ tool: event.target.value })} />
        </label>
      )}
      {(node.type === "tool" || node.type === "mcp_tool") && (
        <label>
          输入 JSON
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
          {zh.labels.condition}
          <input value={node.condition ?? ""} onChange={(event) => onChange({ condition: event.target.value })} />
        </label>
      )}
      {node.type === "human_gate" && (
        <label>
          {zh.labels.approvalReason}
          <textarea
            value={node.approval_reason ?? ""}
            onChange={(event) => onChange({ approval_reason: event.target.value })}
            rows={3}
          />
        </label>
      )}
      <label>
        {zh.labels.outputKey}
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
        来源
        <select value={edge.from} onChange={(event) => onChange({ from: event.target.value })}>
          {nodes.map((node) => (
            <option key={node.id} value={node.id}>
              {node.id}
            </option>
          ))}
        </select>
      </label>
      <label>
        目标
        <select value={edge.to} onChange={(event) => onChange({ to: event.target.value })}>
          {nodes.map((node) => (
            <option key={node.id} value={node.id}>
              {node.id}
            </option>
          ))}
        </select>
      </label>
      <label>
        {zh.labels.condition}
        <input
          placeholder="可选，例如 approval.approved == True"
          value={edge.when ?? ""}
          onChange={(event) => onChange({ when: event.target.value })}
        />
      </label>
      <label>
        {zh.labels.priority}
        <input
          type="number"
          value={edge.priority ?? 0}
          onChange={(event) => onChange({ priority: Number(event.target.value) })}
        />
      </label>
      <label>
        {zh.labels.maxTraversals}
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
        {zh.labels.id}
        <input value={agent.id} onChange={(event) => onChange({ id: event.target.value })} />
      </label>
      <label>
        {zh.labels.name}
        <input value={agent.name ?? ""} onChange={(event) => onChange({ name: event.target.value })} />
      </label>
      <label>
        角色
        <input value={agent.role} onChange={(event) => onChange({ role: event.target.value })} />
      </label>
      <label>
        {zh.labels.goal}
        <textarea value={agent.goal} onChange={(event) => onChange({ goal: event.target.value })} rows={3} />
      </label>
      <label>
        {zh.labels.instructions}
        <textarea
          value={agent.instructions}
          onChange={(event) => onChange({ instructions: event.target.value })}
          rows={5}
        />
      </label>
      <label>
        {zh.labels.provider}
        <input value={agent.provider ?? ""} onChange={(event) => onChange({ provider: event.target.value })} />
      </label>
      <label>
        {zh.labels.model}
        <input value={agent.model ?? ""} onChange={(event) => onChange({ model: event.target.value })} />
      </label>
      <label>
        {zh.labels.tools}
        <input value={agent.tools.join(", ")} onChange={(event) => onChange({ tools: csvToList(event.target.value) })} />
      </label>
      <label>
        {zh.labels.outputKey}
        <input value={agent.output_key ?? ""} onChange={(event) => onChange({ output_key: event.target.value })} />
      </label>
      <div className="panel-subtitle">{zh.labels.permissions}</div>
      <label className="checkbox-row">
        <input
          type="checkbox"
          checked={agent.permissions.read_files}
          onChange={(event) => onChange({ permissions: { ...agent.permissions, read_files: event.target.checked } })}
        />
        {zh.permissions.readFiles}
      </label>
      <label className="checkbox-row">
        <input
          type="checkbox"
          checked={agent.permissions.edit_files}
          onChange={(event) => onChange({ permissions: { ...agent.permissions, edit_files: event.target.checked } })}
        />
        {zh.permissions.editFiles}
      </label>
      <label className="checkbox-row">
        <input
          type="checkbox"
          checked={agent.permissions.run_commands}
          onChange={(event) => onChange({ permissions: { ...agent.permissions, run_commands: event.target.checked } })}
        />
        {zh.permissions.runCommands}
      </label>
      <label className="checkbox-row">
        <input
          type="checkbox"
          checked={agent.permissions.use_network}
          onChange={(event) => onChange({ permissions: { ...agent.permissions, use_network: event.target.checked } })}
        />
        {zh.permissions.useNetwork}
      </label>
      <label className="checkbox-row">
        <input
          type="checkbox"
          checked={agent.permissions.requires_approval}
          onChange={(event) =>
            onChange({ permissions: { ...agent.permissions, requires_approval: event.target.checked } })
          }
        />
        {zh.permissions.requiresApproval}
      </label>
      <div className="panel-subtitle">{zh.labels.contextPolicy}</div>
      <label>
        输入键
        <input
          value={agent.context.input_keys.join(", ")}
          onChange={(event) => onChange({ context: { ...agent.context, input_keys: csvToList(event.target.value) } })}
        />
      </label>
      <label>
        摘要键
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
      label: `${node.id}\n${nodeTypeLabel(node.type)}`
    },
    className: `workflow-node node-${node.type}`
  }));
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
