from __future__ import annotations

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any

from .graph import build_graph
from .module_map import build_module_map
from .project_index import annotate_recommendations, recommend_modules
from .runtime import RuntimeEventBus
from .specs import validate_workflow_spec
from .tools.filesystem import resolve_existing_dir, summarize_project


DEFAULT_WORKFLOW = {
    "id": "coding-review",
    "name": "Default Coding Workflow",
    "description": "Map project, plan with Planner Agent, review with Reviewer Agent, then stop before mutation.",
    "max_loops": 3,
    "agents": [
        {
            "id": "planner",
            "name": "Planner",
            "role": "Planner Agent",
            "goal": "Create a short, scoped implementation plan.",
            "instructions": "Use project context and selected constraints. Return compact JSON. Do not write code.",
            "skills": ["read_project_index", "reason_about_scope", "produce_plan"],
            "input_keys": ["user_request", "modules", "allowed_paths", "repo_files"],
            "output_schema": {
                "summary": "string",
                "target_files": "list[string]",
                "steps": "list[string]",
                "risks": "list[string]",
                "checks": "list[string]",
                "needs_human": "boolean",
            },
            "stop_rules": ["Do not request broad edits", "Ask for approval when dependencies or config must change"],
            "model": None,
            "tools": [],
        },
        {
            "id": "reviewer",
            "name": "Reviewer",
            "role": "Reviewer Agent",
            "goal": "Reject scope escape, high risk, and unclear plans.",
            "instructions": "Review plans and future patches against scope, risk, and stop rules. Return compact JSON.",
            "skills": ["detect_scope_escape", "assess_risk", "produce_stop_reasons"],
            "input_keys": ["user_request", "planner_result", "allowed_paths", "modules"],
            "output_schema": {
                "approved": "boolean",
                "risk_level": "low|medium|high",
                "scope_escape": "boolean",
                "stop_reasons": "list[string]",
                "notes": "string",
            },
            "stop_rules": ["Block scope escape", "Block high-risk changes without human approval"],
            "model": None,
            "tools": [],
        },
    ],
    "steps": [
        {"id": "scan", "kind": "deterministic", "uses": "scan_repo_node", "input_keys": ["repo_root"], "output_key": "repo_files"},
        {"id": "map", "kind": "deterministic", "uses": "module_map_node", "input_keys": ["repo_files"], "output_key": "modules"},
        {"id": "plan", "kind": "agent", "uses": "planner", "input_keys": ["user_request", "modules"], "output_key": "planner_result"},
        {"id": "review", "kind": "agent", "uses": "reviewer", "input_keys": ["planner_result"], "output_key": "reviewer_result"},
        {"id": "approve", "kind": "human_gate", "uses": "approval_node", "input_keys": ["planner_result", "reviewer_result"], "output_key": "approved"},
    ],
    "edges": [
        {"source": "scan", "target": "map"},
        {"source": "map", "target": "plan"},
        {"source": "plan", "target": "review"},
        {"source": "review", "target": "approve"},
    ],
    "stop_conditions": ["scope_escape", "risk_level == high", "max_loops reached"],
}


def run_server(host: str, port: int) -> None:
    server = ThreadingHTTPServer((host, port), CoderWebHandler)
    print(f"Coder local web UI: http://{host}:{port}")
    server.serve_forever()


class CoderWebHandler(BaseHTTPRequestHandler):
    server_version = "CoderWeb/0.2"

    def do_GET(self) -> None:  # noqa: N802
        if self.path in {"/", "/index.html"}:
            self._send_html(INDEX_HTML)
            return
        if self.path == "/api/workflow":
            self._send_json({"workflow": validate_workflow_spec(DEFAULT_WORKFLOW)})
            return
        self.send_error(404)

    def do_POST(self) -> None:  # noqa: N802
        try:
            body = self._read_json()
            if self.path == "/api/select-folder":
                self._send_json({"path": _select_folder()})
                return
            if self.path == "/api/analyze":
                self._send_json(_analyze(body))
                return
            if self.path == "/api/run":
                self._send_json(_run_workflow(body))
                return
            self.send_error(404)
        except Exception as exc:  # pragma: no cover - defensive web boundary
            self._send_json({"error": str(exc)}, status=400)

    def log_message(self, format: str, *args: Any) -> None:
        return

    def _read_json(self) -> dict[str, Any]:
        length = int(self.headers.get("content-length", "0"))
        raw = self.rfile.read(length).decode("utf-8") if length else "{}"
        return json.loads(raw or "{}")

    def _send_json(self, data: dict[str, Any], status: int = 200) -> None:
        payload = json.dumps(data, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("content-type", "application/json; charset=utf-8")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _send_html(self, html: str) -> None:
        payload = html.encode("utf-8")
        self.send_response(200)
        self.send_header("content-type", "text/html; charset=utf-8")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def _analyze(body: dict[str, Any]) -> dict[str, Any]:
    repo = str(body.get("repo", "")).strip()
    query = str(body.get("query", "")).strip()

    if not repo:
        return {"repo": "", "modules": [], "recommendations": [], "workflow": validate_workflow_spec(DEFAULT_WORKFLOW)}

    repo_root = resolve_existing_dir(repo)
    files = summarize_project(repo_root, [], max_files=800)
    modules = build_module_map(files)
    recommendations = recommend_modules(query, modules, files) if query else []
    modules = annotate_recommendations(modules, recommendations) if recommendations else modules
    return {
        "repo": str(repo_root),
        "modules": modules,
        "tree": _build_project_tree(files),
        "recommendations": recommendations,
        "workflow": validate_workflow_spec(DEFAULT_WORKFLOW),
    }


def _run_workflow(body: dict[str, Any]) -> dict[str, Any]:
    repo = str(body.get("repo", "")).strip()
    request = str(body.get("request", "")).strip() or "Analyze this project and propose a safe improvement plan."
    if not repo:
        return {"error": "Project path is empty. Select a project folder before running."}

    state = {
        "user_request": request,
        "repo_root": str(resolve_existing_dir(repo)),
        "reference_roots": [],
        "target_scope": [],
        "allowed_paths": [],
        "check_command": "",
        "approved": False,
        "max_iterations": 3,
    }
    events = RuntimeEventBus()
    events.emit("ui", "status", "Workflow requested", status="running", payload={"repo": repo})
    result = build_graph(event_bus=events).invoke(state)
    events.emit("ui", "result", "Workflow finished", status=str(result.get("status", "unknown")))
    return {
        "plan": result.get("plan", ""),
        "review": result.get("review_notes", ""),
        "status": {
            "status": result.get("status"),
            "risk_level": result.get("risk_level"),
            "check_passed": result.get("check_passed"),
            "changed_files": result.get("changed_files", []),
        },
        "events": events.dump(),
    }


def _select_folder() -> str:
    try:
        import tkinter as tk
        from tkinter import filedialog

        root = tk.Tk()
        root.withdraw()
        root.attributes("-topmost", True)
        selected = filedialog.askdirectory(title="Select project folder")
        root.destroy()
        return selected or ""
    except Exception as exc:  # pragma: no cover - depends on desktop availability
        return f"ERROR: {exc}"


def _build_project_tree(files: list[dict]) -> dict[str, Any]:
    root: dict[str, Any] = {"name": ".", "path": "", "type": "dir", "children": {}, "file_count": 0}
    for item in files:
        parts = item["path"].replace("\\", "/").split("/")
        node = root
        node["file_count"] += 1
        current_path: list[str] = []
        for index, part in enumerate(parts):
            current_path.append(part)
            is_file = index == len(parts) - 1
            children = node.setdefault("children", {})
            if part not in children:
                children[part] = {
                    "name": part,
                    "path": "/".join(current_path),
                    "type": "file" if is_file else "dir",
                    "children": {},
                    "file_count": 0,
                }
            node = children[part]
            node["file_count"] = int(node.get("file_count", 0)) + 1
    return _sort_tree(root)


def _sort_tree(node: dict[str, Any]) -> dict[str, Any]:
    children = node.get("children", {})
    if isinstance(children, dict):
        sorted_children = sorted(
            (_sort_tree(child) for child in children.values()),
            key=lambda child: (child["type"] == "file", child["name"].lower()),
        )
        node["children"] = sorted_children
    return node


INDEX_HTML = r"""<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Coder Local UI</title>
  <style>
    :root { color-scheme: dark; }
    * { box-sizing: border-box; }
    body { margin:0; font-family: ui-sans-serif, system-ui, "Segoe UI", sans-serif; background:#0f172a; color:#e5e7eb; }
    header { padding:18px 24px; background:#111827; border-bottom:1px solid #1f2937; }
    h1 { margin:0 0 6px; font-size:24px; }
    h2 { margin:0 0 12px; font-size:18px; }
    h3 { margin:0 0 8px; font-size:15px; }
    p { color:#94a3b8; line-height:1.5; }
    main { display:grid; grid-template-rows: 1fr auto; min-height:calc(100vh - 82px); }
    .workspace { display:grid; grid-template-columns: 300px minmax(620px,1fr) 320px; gap:14px; padding:14px; min-height:0; }
    .panel { border:1px solid #334155; background:#111827; border-radius:16px; padding:14px; min-height:0; }
    label { display:block; margin-top:12px; color:#cbd5e1; font-size:13px; }
    input, textarea, select { width:100%; margin-top:6px; padding:10px; border-radius:10px; border:1px solid #334155; background:#020617; color:#e5e7eb; }
    textarea { min-height:88px; resize:vertical; }
    button { margin-top:10px; width:100%; border:0; border-radius:10px; padding:10px 12px; background:#2563eb; color:white; cursor:pointer; }
    button.secondary { background:#334155; }
    .modules { position:relative; height:calc(100vh - 205px); overflow:hidden; border:1px solid #1f2937; border-radius:14px; background:#020617; cursor:grab; }
    .modules:active { cursor:grabbing; }
    .graph-space { position:absolute; width:2200px; height:1500px; transform-origin:0 0; }
    .graph-lines { position:absolute; inset:0; width:2200px; height:1500px; pointer-events:none; }
    .graph-node { position:absolute; width:170px; min-height:58px; border:1px solid #334155; border-radius:14px; padding:10px; background:#0f172a; box-shadow:0 10px 26px rgba(0,0,0,.22); cursor:grab; user-select:none; }
    .graph-node:hover { border-color:#38bdf8; }
    .graph-node.dir { background:#111827; }
    .graph-node.file { opacity:.82; }
    .graph-node.omitted { border-style:dashed; color:#94a3b8; background:#020617; }
    .graph-node .name { font-weight:650; word-break:break-word; }
    .graph-node .meta { color:#94a3b8; font-size:12px; margin-top:6px; }
    .graph-toolbar { position:absolute; left:10px; top:10px; z-index:3; display:flex; gap:8px; }
    .graph-toolbar button { width:auto; margin:0; padding:7px 10px; background:#334155; }
    .badge { display:inline-block; border-radius:999px; padding:3px 7px; margin:6px 4px 0 0; font-size:12px; border:1px solid #475569; }
    .high { background:#7f1d1d; color:#fecaca; } .medium { background:#713f12; color:#fde68a; } .low { background:#064e3b; color:#bbf7d0; }
    .agent-list { display:flex; flex-wrap:wrap; gap:10px; align-content:flex-start; min-height:120px; border:1px dashed #334155; border-radius:14px; padding:10px; background:#020617; }
    .agent { width:150px; min-height:86px; border:1px solid #334155; border-radius:14px; padding:10px; background:#0f172a; cursor:grab; user-select:none; }
    .agent.selected { border-color:#38bdf8; }
    .canvas { position:relative; height:calc(100vh - 205px); margin-top:12px; border:1px dashed #334155; border-radius:14px; padding:10px; background:#020617; overflow:hidden; }
    .agent-canvas-svg { position:absolute; inset:0; width:100%; height:100%; pointer-events:auto; }
    .agent-edge { cursor:pointer; pointer-events:stroke; }
    .agent-edge:hover { stroke:#ef4444; stroke-width:4; }
    .canvas .agent { position:absolute; cursor:pointer; }
    .config { white-space:pre-wrap; background:#020617; border:1px solid #334155; padding:12px; border-radius:12px; max-height:260px; overflow:auto; }
    .tabs { display:flex; gap:8px; margin-bottom:10px; }
    .tab { width:auto; margin:0; padding:8px 12px; background:#334155; }
    .tab.active { background:#2563eb; }
    .page { display:none; }
    .page.active { display:block; }
    .connection-mode { border-color:#f59e0b !important; box-shadow:0 0 0 1px rgba(245,158,11,.35); }
    .bottom { border-top:1px solid #1f2937; background:#111827; padding:14px; display:grid; grid-template-columns: minmax(300px,1fr) 220px 420px; gap:14px; align-items:end; }
    pre { white-space:pre-wrap; background:#020617; border:1px solid #334155; padding:12px; border-radius:12px; max-height:180px; overflow:auto; margin:0; }
  </style>
</head>
<body>
  <header>
    <h1>Coder Local UI</h1>
    <p>本地、小巧、模块化的 agent 工作流界面。选择项目 → 查看模块地图 → 搭配 agent → 输入需求 → 运行。</p>
  </header>
  <main>
    <div class="workspace">
      <section class="panel">
        <h2>项目</h2>
        <label>项目文件夹路径（可为空）</label>
        <input id="repo" placeholder="F:\bbb\coder 或 D:\projects\app" />
        <button onclick="selectFolder()">选择文件夹</button>
        <button class="secondary" onclick="analyze()">刷新模块地图</button>
        <label>模式</label>
        <select id="mode" onchange="renderAgents()">
          <option value="default">默认工作流</option>
          <option value="automation">自动化：自己组合</option>
        </select>
        <label>导入 workflow / agent JSON</label>
        <input id="importFile" type="file" accept=".json,application/json" onchange="importWorkflow(event)" />
        <p>限制范围不让用户手填。后续由模块选择、Project Index 和 Reviewer Agent 自动生成与审查边界。</p>
      </section>

      <section class="panel">
        <div class="tabs">
          <button id="projectTab" class="tab active" onclick="switchPage('project')">项目图谱</button>
          <button id="workflowTab" class="tab" onclick="switchPage('workflow')">当前工作流</button>
        </div>
        <div id="projectPage" class="page active">
          <div id="modules" class="modules"></div>
        </div>
        <div id="workflowPage" class="page">
        <h3>当前工作流画布</h3>
        <p>拖动 agent 调整位置。点击“连接 agent”后，依次点击起点和终点创建箭头。点击箭头可删除。</p>
        <button class="secondary" id="connectButton" onclick="toggleConnectMode()">连接 agent：关闭</button>
          <div id="agentCanvas" class="canvas" ondragover="event.preventDefault()" ondrop="dropToCanvas(event)"></div>
        </div>
      </section>

      <section class="panel">
        <h2>Agent 库</h2>
        <div id="agentList" class="agent-list" ondragover="event.preventDefault()" ondrop="dropToList(event)"></div>
        <h3 style="margin-top:14px">Agent 配置 / Skills</h3>
        <pre id="agentConfig" class="config">点击一个 agent 查看配置。</pre>
      </section>
    </div>

    <div class="bottom">
      <div>
        <label>需求 / Query</label>
        <textarea id="request" placeholder="例如：优化聊天记录搜索"></textarea>
      </div>
      <div>
        <button onclick="analyze()">根据需求推荐模块</button>
        <button class="secondary" onclick="runWorkflow()">运行当前工作流</button>
      </div>
      <div>
        <label>运行结果</label>
        <pre id="result">等待运行。</pre>
      </div>
    </div>
  </main>

  <script>
    let current = { modules: [], workflow: null };
    let availableAgents = [];
    let canvasAgents = [];
    let agentEdges = [];
    let draggedAgentId = null;
    let selectedAgentForEdge = null;
    let connectMode = false;
    let movingAgentId = null;
    let movingStart = { x: 0, y: 0 };
    let movingAgentStart = { x: 0, y: 0 };
    let expandedPaths = new Set([""]);
    let graphPan = { x: 40, y: 40 };
    let isPanning = false;
    let panStart = { x: 0, y: 0 };
    let graphStart = { x: 0, y: 0 };

    async function post(url, data = {}) {
      const res = await fetch(url, { method:"POST", headers:{"content-type":"application/json"}, body:JSON.stringify(data) });
      return await res.json();
    }

    async function selectFolder() {
      const result = await post("/api/select-folder");
      if (result.path && !String(result.path).startsWith("ERROR:")) {
        document.getElementById("repo").value = result.path;
        await analyze();
      } else if (result.path) {
        document.getElementById("result").textContent = result.path;
      }
    }

    async function analyze() {
      const repo = document.getElementById("repo").value;
      const query = document.getElementById("request").value;
      current = await post("/api/analyze", { repo, query });
      if (current.workflow) {
        const agents = current.workflow.agents || [];
        availableAgents = mergeAgents(availableAgents, agents);
        if (!canvasAgents.length) {
          canvasAgents = layoutAgents(agents);
          agentEdges = defaultAgentEdges(canvasAgents);
        }
      }
      renderProjectTree();
      renderAgents();
    }

    async function runWorkflow() {
      document.getElementById("result").textContent = "运行中...";
      const data = await post("/api/run", {
        repo: document.getElementById("repo").value,
        request: document.getElementById("request").value
      });
      document.getElementById("result").textContent = formatRunResult(data);
    }

    function formatRunResult(data) {
      if (data.error) return data.error;
      const events = (data.events || []).map(event => `• [${event.source}] ${event.message} (${event.status || event.type})`).join("\n");
      return `${events}\n\nPLAN\n${data.plan || ""}\n\nREVIEW\n${data.review || ""}\n\nSTATUS\n${JSON.stringify(data.status || {}, null, 2)}`;
    }

    function switchPage(page) {
      document.getElementById("projectPage").classList.toggle("active", page === "project");
      document.getElementById("workflowPage").classList.toggle("active", page === "workflow");
      document.getElementById("projectTab").classList.toggle("active", page === "project");
      document.getElementById("workflowTab").classList.toggle("active", page === "workflow");
    }

    function toggleConnectMode() {
      connectMode = !connectMode;
      selectedAgentForEdge = null;
      document.getElementById("connectButton").textContent = `连接 agent：${connectMode ? "开启，依次点击起点和终点" : "关闭"}`;
      renderAgents();
    }

    async function importWorkflow(event) {
      const file = event.target.files[0];
      if (!file) return;
      const text = await file.text();
      const spec = JSON.parse(text);
      const agents = spec.agents || (spec.id ? [spec] : []);
      availableAgents = mergeAgents(availableAgents, agents);
      renderAgents();
    }

    function mergeAgents(existing, incoming) {
      const byId = new Map(existing.map(agent => [agent.id, agent]));
      incoming.forEach(agent => byId.set(agent.id, agent));
      return [...byId.values()];
    }

    function renderProjectTree() {
      const root = document.getElementById("modules");
      if (!current.tree || !current.tree.children || !current.tree.children.length) {
        root.innerHTML = "<p style='padding:14px'>暂无项目图谱。选择项目后会显示 root 以下至少两层内容。</p>";
        return;
      }
      const graph = buildGraph(current.tree);
      window.__lastGraph = graph;
      root.innerHTML = `
        <div class="graph-toolbar">
          <button onclick="resetGraph()">重置视图</button>
          <button onclick="expandAllVisible()">展开可见</button>
        </div>
        <div id="graphSpace" class="graph-space" style="transform:translate(${graphPan.x}px, ${graphPan.y}px)">
          <svg class="graph-lines">${graph.edges.map(edge => graphEdge(edge)).join("")}</svg>
          ${graph.nodes.map(node => graphNode(node)).join("")}
        </div>`;
      root.onmousedown = startPan;
      root.onmousemove = movePan;
      root.onmouseup = stopPan;
      root.onmouseleave = stopPan;
    }

    function buildGraph(root) {
      const nodes = [];
      const edges = [];
      const levels = new Map();
      walkGraph(root, null, 0, levels, nodes, edges);
      for (const [depth, levelNodes] of levels.entries()) {
        levelNodes.forEach((node, index) => {
          node.x = 80 + depth * 270;
          node.y = 90 + index * 105;
        });
      }
      return { nodes, edges };
    }

    function walkGraph(node, parentId, depth, levels, nodes, edges) {
      const id = node.path || "__root__";
      const graphNode = { id, path: node.path || "", name: node.name || "root", type: node.type, file_count: node.file_count || 0, depth, omitted: false };
      nodes.push(graphNode);
      if (!levels.has(depth)) levels.set(depth, []);
      levels.get(depth).push(graphNode);
      if (parentId) edges.push({ from: parentId, to: id });

      const children = node.children || [];
      if (node.type !== "dir" || !children.length) return;
      const shouldExpand = depth < 2 || expandedPaths.has(node.path || "");
      if (!shouldExpand) {
        const omittedId = `${id}::__omitted`;
        const omitted = { id: omittedId, path: node.path || "", name: `… ${children.length} hidden`, type: "omitted", file_count: children.reduce((sum, child) => sum + (child.file_count || 1), 0), depth: depth + 1, omitted: true };
        nodes.push(omitted);
        if (!levels.has(depth + 1)) levels.set(depth + 1, []);
        levels.get(depth + 1).push(omitted);
        edges.push({ from: id, to: omittedId });
        return;
      }
      children.forEach(child => walkGraph(child, id, depth + 1, levels, nodes, edges));
    }

    function graphNode(node) {
      const icon = node.type === "dir" ? "📁" : node.type === "file" ? "📄" : "⋯";
      return `
        <div class="graph-node ${node.type}" style="left:${node.x}px; top:${node.y}px"
             onclick="selectGraphNode('${escapeAttr(node.path)}', '${node.type}')">
          <div class="name">${icon} ${escapeHtml(node.name)}</div>
          <div class="meta">${node.type === "file" ? escapeHtml(node.path) : `${node.file_count || 0} files`}</div>
        </div>`;
    }

    function graphEdge(edge) {
      const graph = window.__lastGraph || buildGraph(current.tree);
      window.__lastGraph = graph;
      const from = graph.nodes.find(node => node.id === edge.from);
      const to = graph.nodes.find(node => node.id === edge.to);
      if (!from || !to) return "";
      const x1 = from.x + 170, y1 = from.y + 34, x2 = to.x, y2 = to.y + 34;
      return `<path d="M${x1},${y1} C${x1 + 55},${y1} ${x2 - 55},${y2} ${x2},${y2}" stroke="#334155" stroke-width="2" fill="none" />`;
    }

    function selectGraphNode(path, type) {
      if (type !== "file") {
        expandedPaths.add(path || "");
        window.__lastGraph = null;
        renderProjectTree();
      }
    }

    function startPan(event) {
      if (event.target.closest(".graph-node") || event.target.closest(".graph-toolbar")) return;
      isPanning = true;
      panStart = { x: event.clientX, y: event.clientY };
      graphStart = { ...graphPan };
    }

    function movePan(event) {
      if (!isPanning) return;
      graphPan = { x: graphStart.x + event.clientX - panStart.x, y: graphStart.y + event.clientY - panStart.y };
      const space = document.getElementById("graphSpace");
      if (space) space.style.transform = `translate(${graphPan.x}px, ${graphPan.y}px)`;
    }

    function stopPan() { isPanning = false; }

    function resetGraph() {
      expandedPaths = new Set([""]);
      graphPan = { x: 40, y: 40 };
      window.__lastGraph = null;
      renderProjectTree();
    }

    function expandAllVisible() {
      const graph = buildGraph(current.tree);
      graph.nodes.filter(node => node.type === "dir").forEach(node => expandedPaths.add(node.path || ""));
      window.__lastGraph = null;
      renderProjectTree();
    }

    function renderAgents() {
      const list = document.getElementById("agentList");
      const canvas = document.getElementById("agentCanvas");
      const mode = document.getElementById("mode").value;
      if (mode === "default" && current.workflow && !canvasAgents.length) {
        canvasAgents = layoutAgents(current.workflow.agents || []);
        agentEdges = defaultAgentEdges(canvasAgents);
      }
      list.innerHTML = availableAgents.map(agent => agentLibraryCard(agent)).join("");
      canvas.innerHTML = `<svg class="agent-canvas-svg">${agentEdges.map(edge => agentEdge(edge)).join("")}</svg>` + canvasAgents.map(agent => agentCanvasNode(agent)).join("");
    }

    function layoutAgents(agents) {
      return agents.map((agent, index) => ({ ...agent, x: 24 + index * 180, y: 78 + (index % 2) * 28 }));
    }

    function defaultAgentEdges(agents) {
      const edges = [];
      for (let i = 0; i < agents.length - 1; i++) edges.push({ from: agents[i].id, to: agents[i + 1].id });
      return edges;
    }

    function agentLibraryCard(agent) {
      const encoded = encodeURIComponent(JSON.stringify(agent));
      return `
        <article class="agent" draggable="true" onclick="showAgent('${encoded}')" ondragstart="dragAgent('${agent.id}')">
          <strong>${escapeHtml(agent.role || agent.id)}</strong>
          <p>${escapeHtml(agent.goal || "")}</p>
          ${(agent.tools || []).map(tool => `<span class="badge low">${escapeHtml(tool)}</span>`).join("")}
        </article>
      `;
    }

    function agentCanvasNode(agent) {
      const encoded = encodeURIComponent(JSON.stringify(agent));
      const selected = selectedAgentForEdge === agent.id ? " selected" : "";
      return `
        <article class="agent${selected}" style="left:${agent.x || 20}px; top:${agent.y || 20}px"
                 onmousedown="startMoveAgent(event, '${agent.id}')"
                 onclick="selectAgentNode(event, '${agent.id}', '${encoded}')">
          <strong>${escapeHtml(agent.role || agent.id)}</strong>
          <p>${escapeHtml(agent.goal || "")}</p>
          ${(agent.skills || agent.tools || []).slice(0, 3).map(skill => `<span class="badge low">${escapeHtml(skill)}</span>`).join("")}
        </article>
      `;
    }

    function agentEdge(edge) {
      const from = canvasAgents.find(agent => agent.id === edge.from);
      const to = canvasAgents.find(agent => agent.id === edge.to);
      if (!from || !to) return "";
      const x1 = (from.x || 0) + 150, y1 = (from.y || 0) + 42;
      const x2 = (to.x || 0), y2 = (to.y || 0) + 42;
      return `<path class="agent-edge" onclick="deleteAgentEdge('${edge.from}', '${edge.to}')" d="M${x1},${y1} C${x1 + 45},${y1} ${x2 - 45},${y2} ${x2},${y2}" stroke="#38bdf8" stroke-width="2" fill="none" marker-end="url(#agentArrow)" />
        <defs><marker id="agentArrow" markerWidth="10" markerHeight="10" refX="8" refY="3" orient="auto"><path d="M0,0 L0,6 L9,3 z" fill="#38bdf8" /></marker></defs>`;
    }

    function deleteAgentEdge(from, to) {
      agentEdges = agentEdges.filter(edge => !(edge.from === from && edge.to === to));
      renderAgents();
    }

    function selectAgentNode(event, id, encoded) {
      event.stopPropagation();
      showAgent(encoded);
      if (!connectMode) {
        selectedAgentForEdge = id;
        renderAgents();
        return;
      }
      if (!selectedAgentForEdge) {
        selectedAgentForEdge = id;
      } else if (selectedAgentForEdge === id) {
        selectedAgentForEdge = null;
      } else {
        const exists = agentEdges.find(edge => edge.from === selectedAgentForEdge && edge.to === id);
        if (!exists) agentEdges.push({ from: selectedAgentForEdge, to: id });
        selectedAgentForEdge = null;
      }
      renderAgents();
    }

    function startMoveAgent(event, id) {
      if (connectMode) return;
      event.stopPropagation();
      movingAgentId = id;
      movingStart = { x: event.clientX, y: event.clientY };
      const agent = canvasAgents.find(item => item.id === id);
      movingAgentStart = { x: agent?.x || 20, y: agent?.y || 20 };
      document.addEventListener("mousemove", moveAgent);
      document.addEventListener("mouseup", stopMoveAgent);
    }

    function moveAgent(event) {
      if (!movingAgentId) return;
      const agent = canvasAgents.find(item => item.id === movingAgentId);
      if (!agent) return;
      agent.x = Math.max(8, movingAgentStart.x + event.clientX - movingStart.x);
      agent.y = Math.max(8, movingAgentStart.y + event.clientY - movingStart.y);
      renderAgents();
    }

    function stopMoveAgent() {
      movingAgentId = null;
      document.removeEventListener("mousemove", moveAgent);
      document.removeEventListener("mouseup", stopMoveAgent);
    }

    function showAgent(encoded) {
      const agent = JSON.parse(decodeURIComponent(encoded));
      document.getElementById("agentConfig").textContent = JSON.stringify(agent, null, 2);
    }

    function dragAgent(id) { draggedAgentId = id; }

    function dropToCanvas(event) {
      event.preventDefault();
      const agent = availableAgents.find(item => item.id === draggedAgentId);
      if (agent && !canvasAgents.find(item => item.id === agent.id)) {
        const rect = event.currentTarget.getBoundingClientRect();
        canvasAgents.push({ ...agent, x: Math.max(12, event.clientX - rect.left - 75), y: Math.max(12, event.clientY - rect.top - 40) });
        renderAgents();
      }
    }

    function dropToList(event) {
      event.preventDefault();
      canvasAgents = canvasAgents.filter(item => item.id !== draggedAgentId);
      renderAgents();
    }

    function escapeHtml(value) {
      return String(value).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#039;'}[c]));
    }

    function escapeAttr(value) {
      return escapeHtml(value).replace(/`/g, "&#096;");
    }

    analyze();
  </script>
</body>
</html>"""


def main() -> None:
    parser = argparse.ArgumentParser(description="Run Coder local web UI")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8765)
    args = parser.parse_args()
    run_server(args.host, args.port)


if __name__ == "__main__":
    main()
