import { useCallback, useEffect, useMemo, useState } from "react";
import {
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
  deleteRun,
  getAgentRuntimeProfiles,
  getAgentWorkflow,
  getDefaultAgentWorkflow,
  getLibrary,
  getLiveAgentRun,
  getRun,
  getRunEvents,
  getToolResult,
  rollbackPatch,
  saveAgentWorkflow,
  startLiveAgentRun,
  subscribeRunEvents,
  validateAgentWorkflow
} from "./api";
import { defaultPlannerLedAgentWorkflow } from "./examples";
import { ProviderSettingsPanel } from "./components/ProviderSettingsPanel";
import { AgentWorkflowAgentInspector } from "./features/agent-workflow/AgentWorkflowAgentInspector";
import { AgentWorkflowEdgeInspector } from "./features/agent-workflow/AgentWorkflowEdgeInspector";
import { AgentWorkflowValidationPanel } from "./features/agent-workflow/AgentWorkflowValidationPanel";
import { SkillsPanel } from "./features/skills/SkillsPanel";
import { useProviderSettings } from "./hooks/useProviderSettings";
import { useRuntimeInfo } from "./hooks/useRuntimeInfo";
import { enUS } from "./i18n";
import { EventReplayList, hydrateBlobRefs, objectList, objectValue, stringList } from "./runEvents";
import { agentWorkflowTemplateCards, instantiateAgentWorkflowTemplate, type AgentWorkflowTemplateCard } from "./template";
import {
  agentEdgeIdFromIndex,
  agentEdgeIndexFromId,
  cleanAgentWorkflowEdge,
  cloneAgentWorkflow,
  downloadJson,
  formatJson,
  linesToList,
  toAgentFlowEdges,
  toAgentFlowNodes
} from "./workflowGraph";
import type {
  AgentModelTier,
  AgentWorkflowAgent,
  AgentWorkflowEdge,
  AgentWorkflowValidationResult,
  AgentWorkflowSpec,
  LibraryIndex,
  LiveRunDetail,
  RunEvent,
  RunSummaryItem,
  StoredRunDetail
} from "./types";

const t = enUS;
const initialAgentWorkflow = cloneAgentWorkflow(defaultPlannerLedAgentWorkflow);
const appSections = ["workflows", "skills", "runs", "settings"] as const;
type AppSection = (typeof appSections)[number];
type PlannerStrength = "fast" | "balanced" | "strong";

export function App() {
  const [activeSection, setActiveSection] = useState<AppSection>("workflows");
  const [library, setLibrary] = useState<LibraryIndex>({ agents: [], agent_workflows: [] });
  const [agentWorkflow, setAgentWorkflow] = useState<AgentWorkflowSpec>(() => cloneAgentWorkflow(initialAgentWorkflow));
  const [jsonText, setJsonText] = useState(() => formatJson(initialAgentWorkflow));
  const [agentWorkflowValidation, setAgentWorkflowValidation] = useState<AgentWorkflowValidationResult | null>(null);
  const [nodes, setNodes] = useState<FlowNode[]>(() => toAgentFlowNodes(initialAgentWorkflow));
  const [edges, setEdges] = useState<FlowEdge[]>(() => toAgentFlowEdges(initialAgentWorkflow));
  const [selectedAgentWorkflowId, setSelectedAgentWorkflowId] = useState<string | null>("planner");
  const [selectedAgentWorkflowEdgeId, setSelectedAgentWorkflowEdgeId] = useState<string | null>(null);
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
  const [historyQuery, setHistoryQuery] = useState("");
  const [historyStatusFilter, setHistoryStatusFilter] = useState("all");
  const [newAgentRoleCard, setNewAgentRoleCard] = useState("do_work");
  const [newAgentName, setNewAgentName] = useState("");
  const [runtimeProfiles, setRuntimeProfiles] = useState<Record<string, unknown>[] | null>(null);
  const {
    capabilities,
    runHistory,
    liveRuns,
    health,
    roleCards,
    refreshRuntimeInfo
  } = useRuntimeInfo(setStatus);
  const {
    providerSettings,
    providerStatus,
    providerForm,
    updateProviderForm,
    refreshProviderInfo,
    persistProviderSettings,
    runProviderTest
  } = useProviderSettings(setStatus);
  const [selectedRunDetail, setSelectedRunDetail] = useState<StoredRunDetail | LiveRunDetail | null>(null);
  const [selectedRunKind, setSelectedRunKind] = useState<"live" | "stored" | null>(null);
  const [runLoading, setRunLoading] = useState(false);
  const selectedAgentWorkflowAgent = useMemo(
    () => agentWorkflow.agents.find((agent) => agent.id === selectedAgentWorkflowId) ?? null,
    [selectedAgentWorkflowId, agentWorkflow.agents]
  );
  const selectedAgentWorkflowEdge = useMemo(() => {
    if (!selectedAgentWorkflowEdgeId) return null;
    const edgeIndex = agentEdgeIndexFromId(selectedAgentWorkflowEdgeId);
    return edgeIndex === null ? null : agentWorkflow.edges[edgeIndex] ?? null;
  }, [selectedAgentWorkflowEdgeId, agentWorkflow.edges]);
  const primaryPlannerAgent = useMemo(
    () => agentWorkflow.agents.find((agent) => agent.id === agentWorkflow.primary_planner_id) ?? null,
    [agentWorkflow.agents, agentWorkflow.primary_planner_id]
  );
  const filteredRunHistory = useMemo(
    () => filterRunHistory(runHistory, historyQuery, historyStatusFilter),
    [runHistory, historyQuery, historyStatusFilter]
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
      const detail = await getLiveAgentRun(runId);
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
        subscribeToRun(detail.id, liveEventsUrl(detail));
      }
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  function renderWorkflowCanvas(nextAgentWorkflow: AgentWorkflowSpec) {
    setNodes(toAgentFlowNodes(nextAgentWorkflow));
    setEdges(toAgentFlowEdges(nextAgentWorkflow));
  }

  function setCurrentAgentWorkflow(next: AgentWorkflowSpec) {
    const clean = cloneAgentWorkflow(next);
    setAgentWorkflow(clean);
    setJsonText(formatJson(clean));
    setRuntimeProfiles(null);
    renderWorkflowCanvas(clean);
    setSelectedAgentWorkflowId(clean.agents[0]?.id ?? null);
    setSelectedAgentWorkflowEdgeId(null);
  }

  function updateAgentWorkflow(mutator: (current: AgentWorkflowSpec) => AgentWorkflowSpec) {
    const next = cloneAgentWorkflow(mutator(cloneAgentWorkflow(agentWorkflow)));
    setAgentWorkflow(next);
    setJsonText(formatJson(next));
    setAgentWorkflowValidation(null);
    setRuntimeProfiles(null);
    renderWorkflowCanvas(next);
  }

  function updatePlannerStrength(strength: PlannerStrength) {
    const modelTier = modelTierForPlannerStrength(strength);
    updateAgentWorkflow((current) => ({
      ...current,
      agents: current.agents.map((agent) =>
        agent.id === current.primary_planner_id ? { ...agent, model_tier: modelTier } : agent
      )
    }));
  }

  async function refreshRuntimeProfiles() {
    setStatus("Compiling runtime profiles...");
    try {
      const profiles = await getAgentRuntimeProfiles(agentWorkflow);
      setRuntimeProfiles(profiles);
      setStatus(`Compiled ${profiles.length} runtime profile(s).`);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  function useTemplateCard(template: AgentWorkflowTemplateCard) {
    const next = instantiateAgentWorkflowTemplate(template);
    setCurrentAgentWorkflow(next);
    setStatus(`Created from template: ${next.name}`);
  }

  async function loadDefaultAgentWorkflow() {
    setStatus("Loading default Agent workflow...");
    try {
      const payload = await getDefaultAgentWorkflow();
      setCurrentAgentWorkflow(payload.agent_workflow);
      setStatus(`Loaded ${payload.agent_workflow.name}`);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  async function loadAgentWorkflow(workflowId: string) {
    setStatus(`Loading Agent workflow ${workflowId}...`);
    try {
      const agentWorkflow = await getAgentWorkflow(workflowId);
      setCurrentAgentWorkflow(agentWorkflow);
      setStatus(`Loaded Agent workflow ${workflowId}`);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  async function applyJson() {
    try {
      const parsed = JSON.parse(jsonText) as AgentWorkflowSpec;
      setCurrentAgentWorkflow(parsed);
      const validation = await validateAgentWorkflow(parsed);
      setAgentWorkflowValidation(validation);
      setStatus(
        validation.status === "error"
          ? "Agent workflow JSON applied with validation errors. Fix them before saving or running."
          : "Agent workflow JSON applied locally. Save to persist it."
      );
    } catch (error) {
      setStatus(error instanceof Error ? `Invalid Agent workflow JSON: ${error.message}` : "Invalid Agent workflow JSON");
    }
  }

  async function persistWorkflow() {
    try {
      const parsed = JSON.parse(jsonText) as AgentWorkflowSpec;
      const validation = await validateAgentWorkflow(parsed);
      setAgentWorkflowValidation(validation);
      if (validation.status === "error") {
        setCurrentAgentWorkflow(parsed);
        setStatus("Save blocked by Agent workflow validation errors.");
        return;
      }
      const saved = await saveAgentWorkflow(parsed);
      setCurrentAgentWorkflow(saved);
      refreshLibrary();
      setStatus(`Saved Agent workflow ${saved.id}`);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  function exportWorkflow() {
    try {
      const parsed = JSON.parse(jsonText) as AgentWorkflowSpec;
      downloadJson(`${parsed.id || "agent-workflow"}.json`, parsed);
      setStatus(`Exported Agent workflow ${parsed.id || "agent-workflow"}`);
    } catch (error) {
      setStatus(error instanceof Error ? `Cannot export invalid Agent workflow JSON: ${error.message}` : "Cannot export invalid Agent workflow JSON");
    }
  }

  function importWorkflow(file: File | null) {
    if (!file) return;
    file
      .text()
      .then(async (text) => {
        const parsed = JSON.parse(text) as AgentWorkflowSpec;
        setCurrentAgentWorkflow(parsed);
        const validation = await validateAgentWorkflow(parsed);
        setAgentWorkflowValidation(validation);
        setStatus(
          validation.status === "error"
            ? `Imported Agent workflow ${parsed.id} with validation errors`
            : `Imported Agent workflow ${parsed.id}`
        );
      })
      .catch((error) => setStatus(error instanceof Error ? `Import failed: ${error.message}` : "Import failed"));
  }

  function uniqueAgentWorkflowAgentId(current: AgentWorkflowSpec) {
    const used = new Set(current.agents.map((agent) => agent.id));
    let index = current.agents.length + 1;
    let candidate = `agent_${index}`;
    while (used.has(candidate)) {
      index += 1;
      candidate = `agent_${index}`;
    }
    return candidate;
  }

  function addAgentWorkflowAgent() {
    const roleCard = roleCards.find((card) => card.id === newAgentRoleCard) ?? roleCards[0];
    if (!roleCard) {
      setStatus("Role cards are unavailable.");
      return;
    }
    const id = uniqueAgentWorkflowAgentId(agentWorkflow);
    const agent: AgentWorkflowAgent = {
      id,
      name: newAgentName.trim() || roleCard.label,
      role: roleCard.role,
      role_card: roleCard.id,
      model_tier: "standard",
      can_talk_to_human: false,
      capabilities: [...roleCard.default_capabilities]
    };
    updateAgentWorkflow((current) => {
      const layout = { ...(current.ui?.layout ?? {}) };
      const index = current.agents.length;
      layout[id] = { x: 80 + (index % 3) * 280, y: 120 + Math.floor(index / 3) * 170 };
      return {
        ...current,
        agents: [...current.agents, agent],
        ui: { ...(current.ui ?? {}), layout }
      };
    });
    setSelectedAgentWorkflowId(id);
    setSelectedAgentWorkflowEdgeId(null);
    setNewAgentName("");
    setStatus(`Added ${roleCard.label} Agent.`);
  }

  function removeAgentWorkflowAgent(agentId = selectedAgentWorkflowId) {
    if (!agentId) return;
    const target = agentWorkflow.agents.find((agent) => agent.id === agentId);
    if (!target) return;
    if (agentId === agentWorkflow.primary_planner_id) {
      setStatus("Primary Planner cannot be deleted. Assign another Planner first.");
      return;
    }
    updateAgentWorkflow((current) => {
      const layout = { ...(current.ui?.layout ?? {}) };
      delete layout[agentId];
      return {
        ...current,
        agents: current.agents.filter((agent) => agent.id !== agentId),
        edges: current.edges.filter((edge) => edge.from !== agentId && edge.to !== agentId),
        ui: { ...(current.ui ?? {}), layout }
      };
    });
    setSelectedAgentWorkflowId((current) => (current === agentId ? agentWorkflow.primary_planner_id : current));
    setSelectedAgentWorkflowEdgeId(null);
    setStatus(`Deleted Agent ${target.name}.`);
  }

  function updateSelectedAgentWorkflowAgent(patch: Partial<AgentWorkflowAgent>) {
    if (!selectedAgentWorkflowAgent) return;
    updateAgentWorkflow((current) => ({
      ...current,
      agents: current.agents.map((agent) =>
        agent.id === selectedAgentWorkflowAgent.id
          ? {
              ...agent,
              ...patch,
              id: agent.id,
              can_talk_to_human: patch.role && patch.role !== "planner" ? false : patch.can_talk_to_human ?? agent.can_talk_to_human
            }
          : agent
      )
    }));
  }

  function updateSelectedAgentWorkflowEdge(patch: Partial<AgentWorkflowEdge>) {
    if (!selectedAgentWorkflowEdgeId) return;
    const edgeIndex = agentEdgeIndexFromId(selectedAgentWorkflowEdgeId);
    if (edgeIndex === null) return;
    updateAgentWorkflow((current) => ({
      ...current,
      edges: current.edges.map((edge, index) =>
        index === edgeIndex ? cleanAgentWorkflowEdge({ ...edge, ...patch }) : edge
      )
    }));
    setSelectedAgentWorkflowEdgeId(agentEdgeIdFromIndex(edgeIndex));
  }
  const onAgentNodesChange = useCallback(
    (changes: NodeChange[]) => {
      const removedIds = changes.filter((change) => change.type === "remove").map((change) => change.id);
      const blockedPrimaryDelete = removedIds.includes(agentWorkflow.primary_planner_id);
      const removableIds = new Set(removedIds.filter((id) => id !== agentWorkflow.primary_planner_id));
      if (blockedPrimaryDelete) {
        setStatus("Primary Planner cannot be deleted. Assign another Planner first.");
      }
      const allowedChanges = changes.filter((change) => change.type !== "remove" || removableIds.has(change.id));
      setNodes((currentNodes) => {
        const nextNodes = applyNodeChanges(allowedChanges, currentNodes);
        setAgentWorkflow((currentWorkflow) => {
          const layout = { ...(currentWorkflow.ui?.layout ?? {}) };
          const agentIds = new Set(currentWorkflow.agents.map((agent) => agent.id));
          for (const node of nextNodes) {
            if (agentIds.has(node.id)) {
              layout[node.id] = { x: node.position.x, y: node.position.y };
            }
          }
          for (const removedId of removableIds) {
            delete layout[removedId];
          }
          const nextWorkflow = {
            ...currentWorkflow,
            agents: currentWorkflow.agents.filter((agent) => !removableIds.has(agent.id)),
            edges: currentWorkflow.edges.filter((edge) => !removableIds.has(edge.from) && !removableIds.has(edge.to)),
            ui: { ...(currentWorkflow.ui ?? {}), layout }
          };
          setJsonText(formatJson(nextWorkflow));
          setAgentWorkflowValidation(null);
          setSelectedAgentWorkflowId((current) =>
            current && removableIds.has(current) ? nextWorkflow.agents[0]?.id ?? null : current
          );
          setSelectedAgentWorkflowEdgeId((current) => (current && removedIds.length > 0 ? null : current));
          return nextWorkflow;
        });
        return nextNodes;
      });
    },
    [agentWorkflow.primary_planner_id]
  );

  const onAgentEdgesChange = useCallback((changes: EdgeChange[]) => {
    const removedIndexes = new Set(
      changes
        .filter((change) => change.type === "remove")
        .map((change) => agentEdgeIndexFromId(change.id))
        .filter((index): index is number => index !== null)
    );
    setEdges((current) => applyEdgeChanges(changes, current));
    if (removedIndexes.size > 0) {
      setAgentWorkflow((currentWorkflow) => {
        const nextWorkflow = {
          ...currentWorkflow,
          edges: currentWorkflow.edges.filter((_, index) => !removedIndexes.has(index))
        };
        setJsonText(formatJson(nextWorkflow));
        setEdges(toAgentFlowEdges(nextWorkflow));
        setAgentWorkflowValidation(null);
        setSelectedAgentWorkflowEdgeId(null);
        return nextWorkflow;
      });
    }
  }, []);

  function onAgentConnect(connection: Connection) {
    const source = connection.source;
    const target = connection.target;
    if (!source || !target) return;
    if (source === target) {
      setStatus("Agent edges must connect two different Agents.");
      return;
    }
    if (agentWorkflow.edges.some((edge) => edge.from === source && edge.to === target)) {
      setStatus(`Edge ${source} -> ${target} already exists.`);
      return;
    }
    updateAgentWorkflow((current) => ({
      ...current,
      edges: [...current.edges, cleanAgentWorkflowEdge({ from: source, to: target })]
    }));
    setSelectedAgentWorkflowEdgeId(agentEdgeIdFromIndex(agentWorkflow.edges.length));
    setSelectedAgentWorkflowId(null);
    setStatus(`Connected ${source} -> ${target}.`);
  }
  async function runWorkflow(approvedOverride = approved) {
    setRunLoading(true);
    setStatus("Validating Agent workflow...");
    try {
      const parsed = JSON.parse(jsonText) as AgentWorkflowSpec;
      const validation = await validateAgentWorkflow(parsed);
      setAgentWorkflowValidation(validation);
      setCurrentAgentWorkflow(parsed);
      if (validation.status === "error") {
        setStatus("Run blocked by Agent workflow validation errors.");
        return;
      }
      const scopes = linesToList(scopesText);
      setEvents([]);
      setEventCursor(0);
      setEventHasMore(false);
      setEventsLoadingMore(false);
      setActiveRunId(null);
      setStatus(approvedOverride ? "Starting approved Agent workflow run..." : "Starting Agent workflow run...");
      const run = await startLiveAgentRun({
        repo,
        request,
        agent_workflow: parsed,
        approved: approvedOverride,
        scopes
      });
      setActiveRunId(run.run_id);
      setSelectedRunKind("live");
      setSelectedRunDetail(null);
      setStatus(`Live Agent run ${run.run_id}: ${run.status}`);
      subscribeToRun(run.run_id, run.events_url);
      refreshRuntimeInfo();
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    } finally {
      setRunLoading(false);
    }
  }

  async function deleteStoredRun(runId: string) {
    if (!window.confirm(`Delete stored run ${runId}? This also removes blobs no other run references.`)) {
      return;
    }
    setStatus(`Deleting stored run ${runId}...`);
    try {
      const result = await deleteRun(runId);
      setSelectedRunDetail(null);
      setSelectedRunKind(null);
      setEvents([]);
      setEventCursor(0);
      setEventHasMore(false);
      refreshRuntimeInfo();
      setStatus(`Deleted ${result.run_id}; removed ${result.orphan_blobs_removed} orphan blob(s).`);
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

  function liveEventsUrl(detail: LiveRunDetail) {
    return `/api/v2/live-agent-runs/${detail.id}/events`;
  }

  return (
    <div className="app-shell">
      <header className="topbar">
        <div>
          <div className="eyebrow">{t.app.eyebrow}</div>
          <h1>{t.app.title}</h1>
        </div>
        <nav className="top-nav" aria-label="Primary">
          {appSections.map((section) => (
            <button
              className={activeSection === section ? "selected" : ""}
              key={section}
              onClick={() => setActiveSection(section)}
            >
              {sectionLabel(section)}
            </button>
          ))}
        </nav>
        <div className="status">{status}</div>
      </header>

      {activeSection === "workflows" ? (
      <>
      <aside className="sidebar">
        <section className="panel">
          <div className="panel-title">{t.templates.title}</div>
          <div className="template-list">
            {agentWorkflowTemplateCards.map((template) => (
              <TemplateCard key={template.id} template={template} onUse={useTemplateCard} />
            ))}
          </div>
        </section>

        <section className="panel">
          <div className="panel-title">{t.library.title}</div>
          <button onClick={loadDefaultAgentWorkflow}>{t.library.loadExample}</button>
          <button onClick={refreshLibrary}>{t.library.refresh}</button>
          <div className="list">
            {library.agent_workflows.length === 0 ? (
              <div className="muted">{t.library.empty}</div>
            ) : (
              library.agent_workflows.map((item) => (
                <button className="list-item" key={item.id} onClick={() => loadAgentWorkflow(item.id)}>
                  <span>{item.name ?? item.id}</span>
                  <small>{item.agents} agents / {item.edges} edges / {item.max_auto_rounds ?? 3} rounds</small>
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
          <button onClick={() => runWorkflow()} disabled={runLoading}>
            {runLoading ? "Starting..." : t.run.start}
          </button>
        </section>

        <section className="panel">
          <div className="panel-title">Provider Settings</div>
          <ProviderSettingsPanel
            form={providerForm}
            settings={providerSettings}
            status={providerStatus}
            onChange={updateProviderForm}
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
          <div className="history-filters">
            <input
              placeholder="Search runs"
              value={historyQuery}
              onChange={(event) => setHistoryQuery(event.target.value)}
            />
            <select value={historyStatusFilter} onChange={(event) => setHistoryStatusFilter(event.target.value)}>
              <option value="all">All statuses</option>
              <option value="completed">Completed</option>
              <option value="blocked">Blocked</option>
              <option value="failed">Failed</option>
            </select>
          </div>
          <div className="list compact-list">
            {filteredRunHistory.slice(0, 20).map((run) => (
              <button className="list-item" key={run.id} onClick={() => openStoredRun(run.id)}>
                <span>{run.workflow_id}</span>
                <small>
                  {run.status}
                  {run.status_code ? `:${run.status_code}` : ""} / {run.events} events
                </small>
              </button>
            ))}
            {runHistory.length === 0 && <div className="muted">{t.runtime.noStoredRuns}</div>}
            {runHistory.length > 0 && filteredRunHistory.length === 0 && <div className="muted">No runs match the filter.</div>}
          </div>
        </section>
      </aside>

      <main className="workspace">
        <section className="canvas-panel">
          <div className="toolbar">
            <div>
              <strong>{agentWorkflow.name}</strong>
              <span>{agentWorkflow.id}</span>
            </div>
            <div className="node-add-controls">
            </div>
          </div>
          <ReactFlow
            nodes={nodes}
            edges={edges}
            onNodesChange={onAgentNodesChange}
            onEdgesChange={onAgentEdgesChange}
            onConnect={onAgentConnect}
            nodesConnectable
            deleteKeyCode="Backspace"
            onNodeClick={(_, node) => {
              setSelectedAgentWorkflowId(node.id);
              setSelectedAgentWorkflowEdgeId(null);
            }}
            onEdgeClick={(_, edge) => {
              setSelectedAgentWorkflowEdgeId(edge.id);
              setSelectedAgentWorkflowId(null);
            }}
            fitView
          >
            <Background />
            <Controls />
            <MiniMap />
          </ReactFlow>
        </section>

        <section className="editor-panel agent-workflow-panel">
          <div className="panel-title">Agent Workflow</div>
          <div className="agent-workflow-settings">
            <label>
              Workflow Name
              <input
                value={agentWorkflow.name}
                onChange={(event) => updateAgentWorkflow((current) => ({ ...current, name: event.target.value }))}
              />
            </label>
            <label>
              Workflow ID
              <input
                value={agentWorkflow.id}
                onChange={(event) => updateAgentWorkflow((current) => ({ ...current, id: event.target.value }))}
              />
            </label>
            <label>
              Max Auto Rounds
              <input
                type="number"
                min={1}
                max={20}
                value={agentWorkflow.loop_policy.max_auto_rounds}
                onChange={(event) =>
                  updateAgentWorkflow((current) => ({
                    ...current,
                    loop_policy: { ...current.loop_policy, max_auto_rounds: Number(event.target.value) }
                  }))
                }
              />
            </label>
            <label>
              Planner Strength
              <select
                value={plannerStrengthFromTier(primaryPlannerAgent?.model_tier ?? "best")}
                onChange={(event) => updatePlannerStrength(event.target.value as PlannerStrength)}
              >
                <option value="fast">Fast</option>
                <option value="balanced">Balanced</option>
                <option value="strong">Strong</option>
              </select>
            </label>
            <label className="checkbox-row">
              <input
                type="checkbox"
                checked={agentWorkflow.loop_policy.user_can_change}
                onChange={(event) =>
                  updateAgentWorkflow((current) => ({
                    ...current,
                    loop_policy: { ...current.loop_policy, user_can_change: event.target.checked }
                  }))
                }
              />
              User can change rounds
            </label>
            <label className="agent-description-field">
              Description
              <textarea
                value={agentWorkflow.description}
                onChange={(event) => updateAgentWorkflow((current) => ({ ...current, description: event.target.value }))}
                rows={3}
              />
            </label>
          </div>
          <div className="summary-grid agent-policy-summary">
            <span>Only Planner can ask the user</span>
            <span>Workers follow PlannerOrder</span>
            <span>Reviewers return evidence</span>
            <span>Runtime profiles are compiled internally</span>
          </div>
          <AgentWorkflowValidationPanel result={agentWorkflowValidation} />
          <details className="json-details">
            <summary>Runtime Profiles (Advanced)</summary>
            <div className="button-row">
              <button onClick={() => void refreshRuntimeProfiles()}>Compile Runtime Profiles</button>
            </div>
            {runtimeProfiles ? (
              <pre>{JSON.stringify(runtimeProfiles, null, 2)}</pre>
            ) : (
              <div className="muted">No runtime profiles loaded.</div>
            )}
          </details>
          <details className="json-details">
            <summary>AgentWorkflowSpec JSON (Advanced)</summary>
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
          </details>
        </section>
      </main>

      <aside className="inspector">
        <section className="panel">
          <div className="panel-title">Agent Inspector</div>
          {selectedAgentWorkflowAgent ? (
            <AgentWorkflowAgentInspector
              agent={selectedAgentWorkflowAgent}
              capabilities={capabilities}
              roleCards={roleCards}
              isPrimaryPlanner={selectedAgentWorkflowAgent.id === agentWorkflow.primary_planner_id}
              onChange={updateSelectedAgentWorkflowAgent}
            />
          ) : selectedAgentWorkflowEdge ? (
            <AgentWorkflowEdgeInspector
              edge={selectedAgentWorkflowEdge}
              agents={agentWorkflow.agents}
              onChange={updateSelectedAgentWorkflowEdge}
            />
          ) : (
            <div className="muted">Select an Agent or edge.</div>
          )}
        </section>

        <section className="panel">
          <>
              <div className="panel-title">Agent Topology</div>
              <div className="add-agent-card">
                <label>
                  Role
                  <select value={newAgentRoleCard} onChange={(event) => setNewAgentRoleCard(event.target.value)}>
                    {roleCards.map((roleCard) => (
                      <option key={roleCard.id} value={roleCard.id}>
                        {roleCard.label}
                      </option>
                    ))}
                  </select>
                </label>
                <label>
                  Name
                  <input value={newAgentName} onChange={(event) => setNewAgentName(event.target.value)} />
                </label>
              </div>
              <div className="button-row">
                <button disabled={roleCards.length === 0} onClick={addAgentWorkflowAgent}>Add Agent</button>
                <button disabled={!selectedAgentWorkflowAgent} onClick={() => removeAgentWorkflowAgent()}>
                  Delete Agent
                </button>
              </div>
              <div className="list compact-list">
                {agentWorkflow.agents.map((agent) => (
                  <button
                    className={`list-item ${agent.id === selectedAgentWorkflowId ? "selected" : ""}`}
                    key={agent.id}
                    onClick={() => {
                      setSelectedAgentWorkflowId(agent.id);
                      setSelectedAgentWorkflowEdgeId(null);
                    }}
                  >
                    <span>{agent.name}</span>
                    <small>{agent.role_card ?? agent.role}</small>
                  </button>
                ))}
              </div>
              <div className="panel-subtitle">Edges</div>
              <div className="list compact-list">
                {agentWorkflow.edges.map((edge, index) => (
                  <button
                    className={`list-item ${agentEdgeIdFromIndex(index) === selectedAgentWorkflowEdgeId ? "selected" : ""}`}
                    key={`${edge.from}-${edge.to}-${index}`}
                    onClick={() => {
                      setSelectedAgentWorkflowEdgeId(agentEdgeIdFromIndex(index));
                      setSelectedAgentWorkflowId(null);
                    }}
                  >
                    <span>{edge.from} -&gt; {edge.to}</span>
                    <small>{edge.loop ? "loop" : "handoff inferred"}</small>
                  </button>
                ))}
                {agentWorkflow.edges.length === 0 && <div className="muted">No edges yet.</div>}
              </div>
          </>
        </section>

        <section className="panel events-panel">
          <div className="panel-title">{t.events.title}</div>
          <RunDetailCard
            detail={selectedRunDetail}
            kind={selectedRunKind}
            activeRunId={activeRunId}
            onAttach={(runId) => openLiveRun(runId, true)}
            onOpenStored={(runId) => openStoredRun(runId)}
            onDeleteStored={(runId) => deleteStoredRun(runId)}
          />
          <RunSummary
            events={events}
          />
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
            <EventReplayList
              events={events}
              runId={selectedRunKind === "stored" ? selectedRunDetail?.id ?? null : null}
            />
          )}
          {selectedRunKind === "stored" && eventHasMore && (
            <button onClick={loadMoreStoredEvents} disabled={eventsLoadingMore}>
              {eventsLoadingMore ? "Loading events..." : "Load more events"}
            </button>
          )}
        </section>
      </aside>
      </>
      ) : activeSection === "skills" ? (
        <main className="page-main">
          <SkillsPanel onStatus={setStatus} />
        </main>
      ) : activeSection === "runs" ? (
        <main className="page-main page-grid">
          <section className="panel">
            <div className="panel-title">{t.runtime.title}</div>
            <button onClick={refreshRuntimeInfo}>{t.runtime.refresh}</button>
            <div className="summary-grid">
              <span>{health?.status ?? t.runtime.unknown}</span>
              <span>{t.runtime.tools(health?.tools.length ?? 0)}</span>
              <span>{t.runtime.liveRuns(liveRuns.length)}</span>
              <span>{t.runtime.storedRuns(runHistory.length)}</span>
            </div>
            <div className="panel-subtitle">Live</div>
            <div className="list compact-list">
              {liveRuns.map((run) => (
                <button className="list-item" key={run.id} onClick={() => openLiveRun(run.id)}>
                  <span>{run.workflow_id}</span>
                  <small>{run.status} / {run.events} events</small>
                </button>
              ))}
              {liveRuns.length === 0 && <div className="muted">{t.runtime.noLiveRuns}</div>}
            </div>
            <div className="panel-subtitle">{t.runtime.storedHistory}</div>
            <div className="history-filters">
              <input
                placeholder="Search runs"
                value={historyQuery}
                onChange={(event) => setHistoryQuery(event.target.value)}
              />
              <select value={historyStatusFilter} onChange={(event) => setHistoryStatusFilter(event.target.value)}>
                <option value="all">All statuses</option>
                <option value="completed">Completed</option>
                <option value="blocked">Blocked</option>
                <option value="failed">Failed</option>
              </select>
            </div>
            <div className="list compact-list">
              {filteredRunHistory.map((run) => (
                <button className="list-item" key={run.id} onClick={() => openStoredRun(run.id)}>
                  <span>{run.workflow_id}</span>
                  <small>
                    {run.status}
                    {run.status_code ? `:${run.status_code}` : ""} / {run.events} events
                  </small>
                </button>
              ))}
              {runHistory.length === 0 && <div className="muted">{t.runtime.noStoredRuns}</div>}
              {runHistory.length > 0 && filteredRunHistory.length === 0 && <div className="muted">No runs match the filter.</div>}
            </div>
          </section>
          <section className="panel events-panel">
            <div className="panel-title">{t.events.title}</div>
            <RunDetailCard
              detail={selectedRunDetail}
              kind={selectedRunKind}
              activeRunId={activeRunId}
              onAttach={(runId) => openLiveRun(runId, true)}
              onOpenStored={(runId) => openStoredRun(runId)}
              onDeleteStored={(runId) => deleteStoredRun(runId)}
            />
            <RunSummary
              events={events}
            />
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
              <EventReplayList
                events={events}
                runId={selectedRunKind === "stored" ? selectedRunDetail?.id ?? null : null}
              />
            )}
            {selectedRunKind === "stored" && eventHasMore && (
              <button onClick={loadMoreStoredEvents} disabled={eventsLoadingMore}>
                {eventsLoadingMore ? "Loading events..." : "Load more events"}
              </button>
            )}
          </section>
        </main>
      ) : (
        <main className="page-main page-grid">
          <section className="panel">
            <div className="panel-title">Provider Settings</div>
            <ProviderSettingsPanel
              form={providerForm}
              settings={providerSettings}
              status={providerStatus}
              onChange={updateProviderForm}
              onSave={persistProviderSettings}
              onRefresh={refreshProviderInfo}
              onTest={runProviderTest}
            />
          </section>
        </main>
      )}
    </div>
  );
}

function sectionLabel(section: AppSection): string {
  if (section === "workflows") return "Workflows";
  if (section === "skills") return "Extensions";
  if (section === "runs") return "Runs";
  return "Settings";
}

function plannerStrengthFromTier(tier: AgentModelTier | string): PlannerStrength {
  if (tier === "economy") return "fast";
  if (tier === "standard") return "balanced";
  return "strong";
}

function modelTierForPlannerStrength(strength: PlannerStrength): AgentModelTier {
  if (strength === "fast") return "economy";
  if (strength === "balanced") return "standard";
  return "best";
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
  template: AgentWorkflowTemplateCard;
  onUse: (template: AgentWorkflowTemplateCard) => void;
}) {
  const isDefaultCoding = template.id === "default-coding";
  const name = isDefaultCoding ? t.templates.defaultCodingName : t.templates.blankName;
  const purpose = isDefaultCoding ? t.templates.defaultCodingPurpose : t.templates.blankPurpose;
  const approvals =
    template.approvals === "plannerOnlyHuman" ? t.templates.plannerOnlyHuman : t.templates.requiredApprovals;
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
  onOpenStored,
  onDeleteStored
}: {
  detail: StoredRunDetail | LiveRunDetail | null;
  kind: "live" | "stored" | null;
  activeRunId: string | null;
  onAttach: (runId: string) => void;
  onOpenStored: (runId: string) => void;
  onDeleteStored: (runId: string) => void;
}) {
  if (!detail || !kind) return null;
  const result = "result" in detail ? detail.result : null;
  const status = kind === "stored" ? result?.status : (detail as LiveRunDetail).status;
  const events = kind === "stored" ? result?.events.length ?? 0 : (detail as LiveRunDetail).events.length;
  const liveDetail = kind === "live" ? (detail as LiveRunDetail) : null;
  const resultData = objectValue(result?.data);
  const canAttach = liveDetail?.status === "queued" || liveDetail?.status === "running" || liveDetail?.status === "blocked";
  const canApprove = liveDetail?.status === "blocked" && Boolean(liveDetail.approval_required);

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
        {result?.status_code && <span>{result.status_code}</span>}
      </div>
      {result?.status_reason && <div className="muted">Reason: {result.status_reason}</div>}
      <div className="muted">Repo: {detail.repo_root}</div>
      <div className="muted">Request: {detail.request}</div>
      {liveDetail?.stored_run_id && (
        <button onClick={() => onOpenStored(liveDetail.stored_run_id as string)}>
          Open stored result
        </button>
      )}
      {canAttach && (
        <button disabled={activeRunId === detail.id && canApprove} onClick={() => onAttach(detail.id)}>
          {canApprove ? "Reattach blocked run" : "Reattach event stream"}
        </button>
      )}
      {kind === "stored" && <button onClick={() => onDeleteStored(detail.id)}>Delete stored run</button>}
      {liveDetail?.error && <div className="muted">Error: {liveDetail.error}</div>}
      {resultData && <RunDiagnostics data={resultData} />}
    </div>
  );
}

function RunDiagnostics({ data }: { data: Record<string, unknown> }) {
  const graphCache = objectValue(data.graph_run_cache);
  const skillRoutes = objectValue(graphCache?.skill_routes);
  const contextPackets = objectValue(graphCache?.context_packets_v2);
  const tokenLedger = objectList(data.token_ledger);
  const runtimeProfiles = objectList(data.runtime_profiles);
  const agentReports = objectList(data.agent_evaluation_reports);
  const skillReports = objectList(data.skill_evaluation_reports);
  const hasDiagnostics =
    tokenLedger.length > 0 ||
    runtimeProfiles.length > 0 ||
    agentReports.length > 0 ||
    skillReports.length > 0 ||
    Boolean(skillRoutes && Object.keys(skillRoutes).length > 0) ||
    Boolean(contextPackets && Object.keys(contextPackets).length > 0);

  if (!hasDiagnostics) return null;

  return (
    <details className="json-details run-diagnostics">
      <summary>Advanced Run Diagnostics</summary>
      <div className="summary-grid">
        <span>{tokenLedger.length} token entries</span>
        <span>{runtimeProfiles.length} runtime profiles</span>
        <span>{contextPackets ? Object.keys(contextPackets).length : 0} context packets</span>
        <span>{skillRoutes ? Object.keys(skillRoutes).length : 0} skill routes</span>
        <span>{agentReports.length} agent reports</span>
        <span>{skillReports.length} skill reports</span>
      </div>
      <pre>
        {JSON.stringify(
          {
            token_ledger: tokenLedger,
            runtime_profiles: runtimeProfiles,
            context_packets_v2: contextPackets ?? {},
            skill_routes: skillRoutes ?? {},
            agent_evaluation_reports: agentReports,
            skill_evaluation_reports: skillReports
          },
          null,
          2
        )}
      </pre>
    </details>
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

function RunSummary({ events }: { events: RunEvent[] }) {
  const latest = events.at(-1);
  const agentCalls = events.filter((event) => event.type === "agent.called").length;
  const toolCalls = events.filter((event) => event.type === "tool.called").length;
  const selectedEdges = events.filter((event) => event.type === "edge.selected").length;
  const approvalRequests = events.filter((event) => event.type === "approval.required");
  const approvalRecords = events.filter((event) => event.type === "approval.recorded");
  const latestApproval = pendingApprovalEvent(events);
  const isBlocked = latest?.type === "run.blocked";
  const latestPayload = objectValue(latest?.payload);

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
      {!latestApproval && isBlocked && (
        <div className="approval-card">
          <div className="panel-subtitle">Blocked</div>
          <div className="summary-grid">
            <span>{String(latestPayload?.code ?? "blocked")}</span>
            <span>{String(latestPayload?.reason ?? latest?.message ?? "")}</span>
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

function pendingApprovalEvent(events: RunEvent[]): RunEvent | null {
  const latestApproval = [...events].reverse().find((event) => event.type === "approval.required");
  if (!latestApproval) return null;
  const latestRecord = [...events].reverse().find((event) => event.type === "approval.recorded");
  if (!latestRecord) return latestApproval;
  const approvalTime = Date.parse(String(latestApproval.created_at ?? ""));
  const recordTime = Date.parse(String(latestRecord.created_at ?? ""));
  if (Number.isFinite(approvalTime) && Number.isFinite(recordTime) && recordTime >= approvalTime) {
    return null;
  }
  return latestApproval;
}

function filterRunHistory(runs: RunSummaryItem[], query: string, status: string): RunSummaryItem[] {
  const needle = query.trim().toLowerCase();
  return runs.filter((run) => {
    if (status !== "all" && run.status !== status) return false;
    if (!needle) return true;
    return [
      run.id,
      run.workflow_id,
      run.repo_root,
      run.request,
      run.status,
      run.status_code ?? "",
      run.status_reason ?? ""
    ]
      .join(" ")
      .toLowerCase()
      .includes(needle);
  });
}

function isTerminalRunEvent(type: string): boolean {
  return (
    type === "run.completed" ||
    type === "run.failed" ||
    type === "run.blocked" ||
    type === "agent_graph.run.completed" ||
    type === "agent_graph.run.failed" ||
    type === "agent_graph.run.blocked"
  );
}

function statusClass(type: string | undefined): string {
  if (type === "run.completed" || type === "agent_graph.run.completed") return "good";
  if (type === "run.failed" || type === "agent_graph.run.failed") return "bad";
  if (type === "run.blocked" || type === "agent_graph.run.blocked" || type === "approval.required") return "warn";
  return "";
}

