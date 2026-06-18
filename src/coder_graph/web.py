from __future__ import annotations

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, urlparse

from .capabilities import apply_capabilities_to_agent, list_capabilities, recommend_capabilities
from .graph import build_graph
from .models import AgentCard
from .module_map import build_module_map
from .project_index import annotate_recommendations, recommend_modules
from .runtime import RuntimeEventBus
from .specs import validate_workflow_spec
from .storage import list_saved_agents, list_saved_workflows, save_agent, save_workflow
from .tools.filesystem import resolve_existing_dir, summarize_project


DEFAULT_WORKFLOW = {
    "id": "codex-cc-default",
    "name": "Codex + CC Execution Workflow",
    "description": "Codex plans and judges; DeepSeek-backed Claude Code agents execute and test; failures route back to Codex for direction.",
    "max_loops": 3,
    "agents": [
        {
            "id": "codex_planner",
            "name": "Codex Planner",
            "role": "Planning, Judgment, and Direction Agent",
            "goal": "Create the high-level plan, judge test results, and give retry instructions when execution fails.",
            "instructions": "Act like Codex: inspect project structure, choose target files, produce implementation/testing instructions, then judge tester results. If tests pass, end. If tests fail, give concrete retry instructions to the executor. Default retry budget is 3.",
            "skills": ["read_project_index", "reason_about_scope", "produce_plan", "assess_risk", "direct_executor"],
            "input_keys": ["user_request", "modules", "allowed_paths", "repo_files", "test_result", "check_output"],
            "output_schema": {
                "summary": "string",
                "target_files": "list[string]",
                "steps": "list[string]",
                "risks": "list[string]",
                "checks": "list[string]",
                "needs_human": "boolean",
            },
            "stop_rules": ["Do not request broad edits", "End when tests pass", "Retry executor on actionable test failures", "Block high-risk or unclear failures"],
            "model": "codex",
            "tools": ["claude_code"],
            "runtime": {
                "enabled": True,
                "page": "agent_workbench",
                "session_id": None,
                "provider": "openai",
                "model": "codex",
                "system_prompt": "Plan the work, judge test results, and give exact retry instructions. Do not mutate files directly. Default retry budget is 3.",
                "context_files": [],
                "mcp_servers": [],
                "skills": ["read_project_index", "reason_about_scope", "produce_plan", "assess_risk", "direct_executor"],
                "tools": ["read", "search", "shell"],
                "enabled_capabilities": ["project-planning"],
                "permissions": {
                    "read_files": True,
                    "edit_files": False,
                    "run_commands": False,
                    "use_network": False,
                    "requires_approval": True,
                },
                "memory": {},
            },
            "a2a": {
                "enabled": True,
                "endpoint": "local://agent/codex_planner",
                "protocol_version": "local-a2a-v1",
                "input_modes": ["application/json"],
                "output_modes": ["application/json"],
                "message_types": ["context.modules_ready", "tester.result"],
                "subscriptions": ["module_map", "cc_tester"],
            },
        },
        {
            "id": "cc_executor",
            "name": "DeepSeek CC Executor",
            "role": "Implementation Agent",
            "goal": "Use Claude Code-style tools with a DeepSeek model to make the concrete code changes requested by Codex.",
            "instructions": "Follow Codex Planner instructions exactly. Edit only allowed paths. Return changed files and implementation notes.",
            "skills": ["apply_scoped_patch", "follow_codex_instructions"],
            "input_keys": ["planner_result", "allowed_paths", "modules"],
            "output_schema": {
                "changed_files": "list[string]",
                "notes": "string",
            },
            "stop_rules": ["Do not edit outside allowed paths", "Stop on ambiguous instructions"],
            "model": "deepseek-chat",
            "tools": ["claude_code"],
            "runtime": {
                "enabled": True,
                "page": "agent_workbench",
                "session_id": None,
                "provider": "deepseek",
                "model": "deepseek-chat",
                "system_prompt": "Execute the approved Codex plan using Claude Code-style local tools. Keep changes scoped.",
                "context_files": [],
                "mcp_servers": [],
                "skills": ["apply_scoped_patch", "follow_codex_instructions"],
                "tools": ["read", "search", "edit", "shell"],
                "enabled_capabilities": ["local-editing"],
                "permissions": {
                    "read_files": True,
                    "edit_files": True,
                    "run_commands": True,
                    "use_network": False,
                    "requires_approval": True,
                },
                "memory": {},
            },
            "a2a": {
                "enabled": True,
                "endpoint": "local://agent/cc_executor",
                "protocol_version": "local-a2a-v1",
                "input_modes": ["application/json"],
                "output_modes": ["application/json"],
                "message_types": ["plan.proposed", "codex.retry_requested"],
                "subscriptions": ["codex_planner"],
            },
        },
        {
            "id": "cc_tester",
            "name": "DeepSeek CC Tester",
            "role": "Testing Agent",
            "goal": "Run or choose validation checks for the executor's changes and summarize failures.",
            "instructions": "Use Claude Code-style command tools to run appropriate checks. Return pass/fail and concise failure evidence.",
            "skills": ["choose_validation_checks", "interpret_check_output"],
            "input_keys": ["changed_files", "planner_result", "allowed_paths"],
            "output_schema": {
                "passed": "boolean",
                "output": "string",
            },
            "stop_rules": ["Do not mutate files while testing", "Return exact failure evidence"],
            "model": "deepseek-chat",
            "tools": ["claude_code"],
            "runtime": {
                "enabled": True,
                "page": "agent_workbench",
                "session_id": None,
                "provider": "deepseek",
                "model": "deepseek-chat",
                "system_prompt": "Test the executor changes. Prefer project-native checks and concise evidence.",
                "context_files": [],
                "mcp_servers": [],
                "skills": ["choose_validation_checks", "interpret_check_output"],
                "tools": ["read", "search", "shell"],
                "enabled_capabilities": ["local-checks"],
                "permissions": {
                    "read_files": True,
                    "edit_files": False,
                    "run_commands": True,
                    "use_network": False,
                    "requires_approval": True,
                },
                "memory": {},
            },
            "a2a": {
                "enabled": True,
                "endpoint": "local://agent/cc_tester",
                "protocol_version": "local-a2a-v1",
                "input_modes": ["application/json"],
                "output_modes": ["application/json"],
                "message_types": ["executor.changed_files"],
                "subscriptions": ["cc_executor"],
            },
        },
    ],
    "steps": [
        {"id": "scan", "kind": "deterministic", "uses": "scan_repo_node", "input_keys": ["repo_root"], "output_key": "repo_files"},
        {"id": "map", "kind": "deterministic", "uses": "module_map_node", "input_keys": ["repo_files"], "output_key": "modules"},
        {"id": "plan", "kind": "agent", "uses": "codex_planner", "input_keys": ["user_request", "modules"], "output_key": "planner_result"},
        {"id": "execute", "kind": "agent", "uses": "cc_executor", "input_keys": ["planner_result"], "output_key": "execution_result"},
        {"id": "test", "kind": "agent", "uses": "cc_tester", "input_keys": ["execution_result"], "output_key": "test_result"},
        {"id": "judge", "kind": "agent", "uses": "codex_planner", "input_keys": ["test_result"], "output_key": "judge_result"},
    ],
    "edges": [
        {"source": "scan", "target": "map"},
        {"source": "map", "target": "plan"},
        {"source": "plan", "target": "execute"},
        {"source": "execute", "target": "test"},
        {"source": "test", "target": "judge"},
        {"source": "judge", "target": "execute", "condition": "retry"},
    ],
    "stop_conditions": ["tests pass", "scope_escape", "risk_level == high", "max_loops reached"],
}


def run_server(host: str, port: int) -> None:
    server = ThreadingHTTPServer((host, port), CoderWebHandler)
    print(f"Coder local web UI: http://{host}:{port}")
    server.serve_forever()


class CoderWebHandler(BaseHTTPRequestHandler):
    server_version = "CoderWeb/0.2"

    def do_GET(self) -> None:  # noqa: N802
        parsed = urlparse(self.path)
        if parsed.path in {"/", "/index.html"}:
            self._send_html(INDEX_HTML)
            return
        if parsed.path == "/api/workflow":
            self._send_json({"workflow": validate_workflow_spec(DEFAULT_WORKFLOW)})
            return
        if parsed.path == "/api/library":
            query = parse_qs(parsed.query)
            repo = query.get("repo", [""])[0]
            self._send_json(_load_library(repo))
            return
        if parsed.path == "/api/capabilities":
            self._send_json({"capabilities": list_capabilities()})
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
            if self.path == "/api/save-workflow":
                self._send_json(_save_workflow(body))
                return
            if self.path == "/api/save-agent":
                self._send_json(_save_agent(body))
                return
            if self.path == "/api/recommend-capabilities":
                self._send_json(
                    {
                        "capabilities": recommend_capabilities(
                            query=str(body.get("query", "")),
                            agent=body.get("agent") or {},
                            modules=body.get("modules") or [],
                        )
                    }
                )
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
        return {
            "repo": "",
            "modules": [],
            "recommendations": [],
            "workflow": validate_workflow_spec(DEFAULT_WORKFLOW),
            "library": {"workflows": [], "agents": []},
        }

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
        "library": _load_library(str(repo_root)),
    }


def _load_library(repo: str) -> dict[str, Any]:
    if not repo:
        return {"workflows": [], "agents": []}
    return {
        "workflows": list_saved_workflows(repo),
        "agents": list_saved_agents(repo),
    }


def _save_workflow(body: dict[str, Any]) -> dict[str, Any]:
    repo = str(body.get("repo", "")).strip()
    workflow = body.get("workflow") or {}
    if not repo:
        return {"error": "Project path is empty. Select a project folder before saving."}
    saved = save_workflow(repo, workflow)
    return {"workflow": saved, "library": _load_library(repo)}


def _save_agent(body: dict[str, Any]) -> dict[str, Any]:
    repo = str(body.get("repo", "")).strip()
    agent = body.get("agent") or {}
    capability_ids = body.get("capabilities") or agent.get("runtime", {}).get("enabled_capabilities", [])
    if not repo:
        return {"error": "Project path is empty. Select a project folder before saving."}
    agent = apply_capabilities_to_agent(agent, capability_ids)
    saved = save_agent(repo, agent)
    return {"agent": saved, "library": _load_library(repo)}


def _run_workflow(body: dict[str, Any]) -> dict[str, Any]:
    repo = str(body.get("repo", "")).strip()
    request = str(body.get("request", "")).strip() or "Analyze this project and propose a safe improvement plan."
    if not repo:
        return {"error": "Project path is empty. Select a project folder before running."}
    repo_root = resolve_existing_dir(repo)
    files = summarize_project(repo_root, [], max_files=800)
    modules = build_module_map(files)
    run_settings = _infer_run_settings(repo_root, request, modules, files)

    state = {
        "user_request": request,
        "repo_root": str(repo_root),
        "reference_roots": [],
        "target_scope": run_settings["target_scope"],
        "allowed_paths": run_settings["allowed_paths"],
        "check_command": run_settings["check_command"],
        "approved": True,
        "max_iterations": run_settings["max_iterations"],
    }
    agents = [AgentCard.model_validate(agent) for agent in DEFAULT_WORKFLOW["agents"]]
    events = RuntimeEventBus(agents=agents)
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
        "messages": events.dump_messages(),
        "a2a_queues": events.dump_a2a_queues(),
        "run_settings": run_settings,
    }


def _infer_run_settings(repo_root: Path, request: str, modules: list[dict], files: list[dict]) -> dict[str, Any]:
    recommendations = recommend_modules(request, modules, files) if request else []
    target_scope = [item["path"] for item in recommendations[:2] if item.get("path")]
    return {
        "target_scope": target_scope,
        "allowed_paths": target_scope,
        "check_command": _detect_check_command(repo_root, files),
        "max_iterations": 3,
        "source": "auto",
    }


def _detect_check_command(repo_root: Path, files: list[dict]) -> str:
    paths = {item["path"] for item in files}
    package_json = repo_root / "package.json"
    if "package.json" in paths and package_json.exists():
        try:
            package = json.loads(package_json.read_text(encoding="utf-8"))
            scripts = package.get("scripts", {})
            for script in ["test", "typecheck", "lint", "build"]:
                if script in scripts:
                    return f"npm run {script}"
        except json.JSONDecodeError:
            pass
    if "pyproject.toml" in paths or any(path.startswith("src/") and path.endswith(".py") for path in paths):
        return "python -m compileall src"
    return ""


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
    .graph-node { position:absolute; width:170px; min-height:58px; border:1px solid #334155; border-radius:14px; padding:10px; background:#0f172a; box-shadow:0 10px 26px rgba(0,0,0,.22); cursor:pointer; user-select:none; }
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
    .agent-picker { display:none; position:absolute; z-index:6; width:320px; max-height:320px; overflow:auto; margin-top:8px; padding:10px; border:1px solid #334155; border-radius:14px; background:#020617; box-shadow:0 18px 50px rgba(0,0,0,.35); }
    .agent-picker.open { display:block; }
    .workflow-actions { display:flex; gap:8px; align-items:center; margin:10px 0 8px; }
    .workflow-actions button { flex:1; width:auto; margin:0; }
    .agent-option { display:flex; align-items:flex-start; gap:8px; padding:8px; border-radius:10px; }
    .agent-option:hover { background:#1e293b; }
    .agent-option input { width:auto; margin-top:3px; }
    .agent-option strong { display:block; }
    .agent-option span { display:block; color:#94a3b8; font-size:12px; margin-top:3px; }
    .modal-backdrop { display:none; position:fixed; inset:0; z-index:20; background:rgba(2,6,23,.72); align-items:center; justify-content:center; }
    .modal-backdrop.open { display:flex; }
    .modal { width:min(760px, calc(100vw - 48px)); max-height:calc(100vh - 70px); overflow:auto; border:1px solid #334155; border-radius:18px; background:#111827; padding:16px; box-shadow:0 24px 80px rgba(0,0,0,.45); }
    .modal textarea { min-height:360px; font-family:ui-monospace, SFMono-Regular, Consolas, monospace; }
    .modal-actions { display:flex; gap:10px; justify-content:flex-end; }
    .modal-actions button { width:auto; min-width:90px; }
    .workbench-grid { display:grid; grid-template-columns: 1fr 1fr; gap:12px; }
    .workbench-card { border:1px solid #334155; border-radius:14px; background:#020617; padding:12px; }
    .workbench-card h3 { margin-bottom:10px; }
    .workbench-card label { margin-top:8px; }
    .workbench-header { display:flex; align-items:flex-start; justify-content:space-between; gap:16px; margin-bottom:12px; }
    .workbench-header p { margin:6px 0 0; }
    .workbench-close { width:auto; margin:0; background:#334155; }
    .checkbox-row { display:grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap:6px 10px; margin-top:8px; }
    .checkbox-row label { display:flex; align-items:center; gap:6px; margin:0; }
    .checkbox-row input { width:auto; margin:0; }
    .capability-grid { display:grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap:10px; margin-top:8px; }
    .capability-card { border:1px solid #334155; border-radius:12px; background:#0f172a; padding:10px; }
    .capability-card.recommended { border-color:#38bdf8; }
    .capability-card label { display:flex; align-items:flex-start; gap:8px; margin:0; }
    .capability-card input { width:auto; margin:3px 0 0; }
    .capability-card strong { display:block; }
    .capability-card span { display:block; color:#94a3b8; font-size:12px; margin-top:4px; }
    .capability-meta { display:flex; gap:6px; flex-wrap:wrap; margin-top:6px; }
    .workbench-wide { grid-column: 1 / -1; }
    details.workbench-card summary { cursor:pointer; font-weight:650; }
    .agent-workbench-overlay { display:none; position:fixed; inset:18px; z-index:18; overflow:auto; border:1px solid #334155; border-radius:18px; background:#111827; padding:16px; box-shadow:0 24px 80px rgba(0,0,0,.55); }
    .agent-workbench-overlay.open { display:block; }
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
        <select id="mode" onchange="selectWorkflowMode()">
          <option value="default">默认工作流</option>
        </select>
        <label>导入 workflow / agent JSON</label>
        <input id="importFile" type="file" accept=".json,application/json" onchange="importWorkflow(event)" />
        <p>默认工作流会自动选择相关范围、检查命令和重试次数。普通用户只需要选择项目并输入需求。</p>
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
          <div class="workflow-actions">
            <button class="secondary" onclick="toggleAgentPicker()">选择 Agent</button>
            <button class="secondary" id="connectButton" onclick="toggleConnectMode()">连接 agent：关闭</button>
            <button class="secondary" onclick="saveCurrentWorkflow()">保存工作流</button>
          </div>
          <div id="agentPicker" class="agent-picker">
            <div id="agentPickerList"></div>
          </div>
          <div id="agentCanvas" class="canvas" ondragover="event.preventDefault()" ondrop="dropToCanvas(event)"></div>
        </div>
      </section>

      <section class="panel">
        <h2>配置</h2>
        <p>双击当前工作流画布中的 agent，可以查看并编辑 Agent Card。MCP、skills、tools 后续会作为受控能力接入。</p>
        <pre id="agentConfig" class="config">双击一个 agent 查看配置。</pre>
      </section>
    </div>

    <div class="bottom">
      <div>
        <label>需求 / Query</label>
        <textarea id="request" placeholder="例如：优化聊天记录搜索"></textarea>
      </div>
      <div>
        <button class="secondary" onclick="runWorkflow()">运行当前工作流</button>
      </div>
      <div>
        <label>运行结果</label>
        <pre id="result">等待运行。</pre>
      </div>
    </div>
  </main>

  <div id="agentPage" class="agent-workbench-overlay">
    <div class="workbench-header">
      <div>
        <h2>Claude Code Agent 页面</h2>
        <p>普通用户只需要配置角色、模型和权限；MCP、A2A 和完整 JSON 放在高级区域，保存后退出回到当前工作流。</p>
      </div>
      <button class="workbench-close" onclick="closeAgentEditor()">退出</button>
    </div>
    <div class="workbench-grid">
      <section class="workbench-card">
        <h3>Agent</h3>
        <label>ID</label>
        <input id="agentWorkbenchId" disabled />
        <label>角色</label>
        <input id="agentWorkbenchRole" />
        <label>目标</label>
        <textarea id="agentWorkbenchGoal"></textarea>
      </section>
      <section class="workbench-card">
        <h3>Claude Code Runtime</h3>
        <label>Provider</label>
        <input id="agentWorkbenchProvider" placeholder="anthropic / openai-compatible / local" />
        <label>Model</label>
        <input id="agentWorkbenchModel" placeholder="claude / local model name" />
        <label>Session ID</label>
        <input id="agentWorkbenchSession" placeholder="保存后可继续同一 agent 会话" />
      </section>
      <section class="workbench-card workbench-wide">
        <h3>权限</h3>
        <div class="checkbox-row">
          <label><input id="permReadFiles" type="checkbox" />读文件</label>
          <label><input id="permEditFiles" type="checkbox" />改文件</label>
          <label><input id="permRunCommands" type="checkbox" />运行命令</label>
          <label><input id="permUseNetwork" type="checkbox" />网络</label>
          <label><input id="permRequiresApproval" type="checkbox" />关键操作需要批准</label>
        </div>
      </section>
      <section class="workbench-card">
        <h3>能力</h3>
        <label>MCP Servers JSON</label>
        <textarea id="agentWorkbenchMcp"></textarea>
        <label>Skills（逗号分隔）</label>
        <input id="agentWorkbenchSkills" />
        <label>Tools（逗号分隔）</label>
        <input id="agentWorkbenchTools" />
      </section>
      <section class="workbench-card">
        <h3>A2A 协议</h3>
        <label>Endpoint</label>
        <input id="agentWorkbenchA2AEndpoint" />
        <label>Message Types（逗号分隔）</label>
        <input id="agentWorkbenchA2AMessageTypes" />
        <label>Subscriptions（逗号分隔）</label>
        <input id="agentWorkbenchA2ASubscriptions" />
      </section>
      <section class="workbench-card workbench-wide">
        <h3>Capabilities</h3>
        <p>Use capability packs instead of asking users to hand-write skills, tools, MCP, or A2A settings.</p>
        <button class="secondary" onclick="refreshCapabilityRecommendations()">Recommend capabilities</button>
        <div id="capabilityList" class="capability-grid"></div>
      </section>
      <section class="workbench-card workbench-wide">
        <h3>System Prompt / Instructions</h3>
        <textarea id="agentWorkbenchPrompt"></textarea>
      </section>
      <details class="workbench-card workbench-wide">
        <summary>高级 Agent Card JSON</summary>
        <p>仅在需要精确编辑协议字段、runtime memory 或导入外部配置时使用。</p>
        <textarea id="agentEditor"></textarea>
      </details>
    </div>
    <div class="modal-actions">
      <button class="secondary" onclick="closeAgentEditor()">退出</button>
      <button onclick="saveAgentEditor()">保存并退出</button>
    </div>
  </div>

  <script>
    let current = { modules: [], workflow: null };
    let savedWorkflows = [];
    let availableAgents = [];
    let capabilityCatalog = [];
    let recommendedCapabilityIds = new Set();
    let canvasAgents = [];
    let agentEdges = [];
    let draggedAgentId = null;
    let selectedAgentForEdge = null;
    let connectMode = false;
    let agentPickerOpen = false;
    let editingAgentId = null;
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

    async function getJson(url) {
      const res = await fetch(url);
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
      await loadCapabilities();
      current = await post("/api/analyze", { repo, query });
      if (current.workflow) {
        const agents = (current.workflow.agents || []).map(withClaudeCode);
        availableAgents = mergeAgents(availableAgents, agents);
        if (!canvasAgents.length) {
          canvasAgents = layoutAgents(agents);
          agentEdges = defaultAgentEdges(canvasAgents);
        }
      }
      applyLibrary(current.library || { workflows: [], agents: [] });
      renderProjectTree();
      renderAgents();
    }

    async function loadCapabilities() {
      if (capabilityCatalog.length) return;
      const data = await getJson("/api/capabilities");
      capabilityCatalog = data.capabilities || [];
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
      const messages = (data.messages || []).map(message => `→ [${message.sender} -> ${message.recipient}] ${message.type}`).join("\n");
      return `${events}\n\nAUTO SETTINGS\n${JSON.stringify(data.run_settings || {}, null, 2)}\n\nA2A MESSAGES\n${messages || "No messages"}\n\nA2A QUEUES\n${JSON.stringify(data.a2a_queues || {}, null, 2)}\n\nPLAN\n${data.plan || ""}\n\nREVIEW\n${data.review || ""}\n\nSTATUS\n${JSON.stringify(data.status || {}, null, 2)}`;
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

    function toggleAgentPicker() {
      agentPickerOpen = !agentPickerOpen;
      renderAgents();
    }

    async function importWorkflow(event) {
      const file = event.target.files[0];
      if (!file) return;
      const text = await file.text();
      const spec = JSON.parse(text);
      const agents = spec.agents || (spec.id ? [spec] : []);
      availableAgents = mergeAgents(availableAgents, agents.map(withClaudeCode));
      renderAgents();
    }

    function mergeAgents(existing, incoming) {
      const byId = new Map(existing.map(agent => [agent.id, agent]));
      incoming.filter(Boolean).forEach(agent => byId.set(agent.id, withClaudeCode(agent)));
      return [...byId.values()];
    }

    function withClaudeCode(agent) {
      const skills = new Set(agent.skills || []);
      const tools = new Set(agent.tools || []);
      tools.add("claude_code");
      const runtimeTools = new Set(agent.runtime?.tools || ["read", "search", "edit", "shell"]);
      const runtimeSkills = new Set(agent.runtime?.skills || agent.skills || []);
      return {
        ...agent,
        skills: [...skills],
        tools: [...tools],
        runtime: {
          enabled: true,
          page: "agent_workbench",
          session_id: null,
          provider: null,
          model: agent.model || null,
          system_prompt: agent.instructions || "",
          context_files: [],
          mcp_servers: [],
          skills: [...runtimeSkills],
          tools: [...runtimeTools],
          enabled_capabilities: [],
          permissions: {
            read_files: true,
            edit_files: false,
            run_commands: false,
            use_network: false,
            requires_approval: true,
            ...(agent.runtime?.permissions || {})
          },
          memory: {},
          ...(agent.runtime || {})
        },
        a2a: {
          enabled: true,
          endpoint: `local://agent/${agent.id}`,
          protocol_version: "local-a2a-v1",
          input_modes: ["application/json"],
          output_modes: ["application/json"],
          message_types: [],
          subscriptions: [],
          ...(agent.a2a || {})
        }
      };
    }

    function enabledCapabilities(agent) {
      return new Set(agent?.runtime?.enabled_capabilities || []);
    }

    function renderCapabilities(agent) {
      const root = document.getElementById("capabilityList");
      if (!root) return;
      const selected = enabledCapabilities(agent);
      root.innerHTML = capabilityCatalog.map(capability => {
        const checked = selected.has(capability.id) ? "checked" : "";
        const recommended = recommendedCapabilityIds.has(capability.id) ? " recommended" : "";
        const riskClass = capability.risk === "medium" ? "medium" : capability.risk === "high" ? "high" : "low";
        return `
          <article class="capability-card${recommended}">
            <label>
              <input type="checkbox" ${checked} onchange="toggleCapability('${escapeAttr(capability.id)}', this.checked)" />
              <span>
                <strong>${escapeHtml(capability.name)}</strong>
                <span>${escapeHtml(capability.description || "")}</span>
                <span class="capability-meta">
                  <span class="badge ${riskClass}">${escapeHtml(capability.risk || "low")} risk</span>
                  <span class="badge low">${escapeHtml(capability.kind || "capability")}</span>
                  ${recommendedCapabilityIds.has(capability.id) ? '<span class="badge low">recommended</span>' : ''}
                </span>
              </span>
            </label>
          </article>
        `;
      }).join("");
    }

    function toggleCapability(id, checked) {
      const agent = canvasAgents.find(item => item.id === editingAgentId);
      if (!agent) return;
      const runtime = agent.runtime || {};
      const selected = enabledCapabilities(agent);
      if (checked) selected.add(id);
      else selected.delete(id);
      runtime.enabled_capabilities = [...selected];
      agent.runtime = runtime;
      fillAgentWorkbench(agent);
    }

    async function refreshCapabilityRecommendations() {
      await loadCapabilities();
      const agent = canvasAgents.find(item => item.id === editingAgentId) || {};
      const data = await post("/api/recommend-capabilities", {
        query: document.getElementById("request").value,
        agent,
        modules: current.modules || []
      });
      recommendedCapabilityIds = new Set((data.capabilities || []).map(capability => capability.id));
      renderCapabilities(agent);
    }

    function applyCapabilitiesLocally(agent) {
      const selected = enabledCapabilities(agent);
      const updated = withClaudeCode(agent);
      const runtime = updated.runtime || {};
      const permissions = runtime.permissions || {};
      const skills = new Set([...(updated.skills || []), ...(runtime.skills || [])]);
      const runtimeTools = new Set(runtime.tools || []);
      const topTools = new Set(updated.tools || []);
      const mcpServers = [...(runtime.mcp_servers || [])];
      const mcpNames = new Set(mcpServers.map(server => server && server.name).filter(Boolean));
      for (const capability of capabilityCatalog) {
        if (!selected.has(capability.id)) continue;
        (capability.skills || []).forEach(skill => skills.add(skill));
        (capability.tools || []).forEach(tool => {
          runtimeTools.add(tool);
          topTools.add(tool);
        });
        Object.assign(permissions, capability.permissions || {});
        for (const server of capability.mcp_servers || []) {
          if (server.name && !mcpNames.has(server.name)) {
            mcpServers.push(server);
            mcpNames.add(server.name);
          }
        }
      }
      runtime.enabled_capabilities = [...selected];
      runtime.skills = [...skills].sort();
      runtime.tools = [...runtimeTools].sort();
      runtime.mcp_servers = mcpServers;
      runtime.permissions = permissions;
      updated.skills = [...skills].sort();
      updated.tools = [...topTools].sort();
      updated.runtime = runtime;
      return updated;
    }

    function applyLibrary(library) {
      savedWorkflows = library.workflows || [];
      availableAgents = mergeAgents(availableAgents, library.agents || []);
      renderWorkflowModes();
    }

    function renderWorkflowModes() {
      const select = document.getElementById("mode");
      const selected = select.value || "default";
      select.innerHTML = `<option value="default">默认工作流</option>` + savedWorkflows
        .map(workflow => `<option value="${escapeAttr(workflow.id)}">${escapeHtml(workflow.name)}</option>`)
        .join("");
      select.value = savedWorkflows.some(workflow => workflow.id === selected) ? selected : "default";
    }

    function normalizeCanvasEdge(edge) {
      return {
        from: edge.from || edge.source,
        to: edge.to || edge.target,
        condition: edge.condition || null
      };
    }

    function workflowSpecEdge(edge) {
      const normalized = normalizeCanvasEdge(edge);
      return {
        source: normalized.from,
        target: normalized.to,
        condition: normalized.condition
      };
    }

    function withLocalA2ARouting(agent) {
      const normalized = applyCapabilitiesLocally(withClaudeCode(agent));
      const subscriptions = agentEdges
        .filter(edge => normalizeCanvasEdge(edge).to === normalized.id)
        .map(edge => normalizeCanvasEdge(edge).from);
      const messageTypes = subscriptions.map(source => `${source}.result`);
      return {
        ...normalized,
        a2a: {
          ...(normalized.a2a || {}),
          enabled: true,
          endpoint: `local://agent/${normalized.id}`,
          protocol_version: "local-a2a-v1",
          input_modes: ["application/json"],
          output_modes: ["application/json"],
          subscriptions: [...new Set([...(normalized.a2a?.subscriptions || []), ...subscriptions])],
          message_types: [...new Set([...(normalized.a2a?.message_types || []), ...messageTypes])]
        }
      };
    }

    function selectWorkflowMode() {
      const mode = document.getElementById("mode").value;
      if (mode === "default") {
        if (current.workflow) {
          canvasAgents = layoutAgents((current.workflow.agents || []).map(withClaudeCode));
          agentEdges = defaultAgentEdges(canvasAgents);
        }
        renderAgents();
        return;
      }
      const workflow = savedWorkflows.find(item => item.id === mode);
      if (!workflow) return;
      current.workflow = workflow;
      canvasAgents = (workflow.agents || []).map(withClaudeCode);
      agentEdges = (workflow.edges || defaultAgentEdges(canvasAgents)).map(normalizeCanvasEdge);
      availableAgents = mergeAgents(availableAgents, canvasAgents);
      renderAgents();
    }

    function currentWorkflowSpec(name) {
      const base = current.workflow || {};
      const selectedMode = document.getElementById("mode").value;
      const workflowId = selectedMode && selectedMode !== "default" ? selectedMode : `workflow-${Date.now()}`;
      return {
        ...base,
        id: workflowId,
        name,
        description: base.description || "User-composed workflow",
        agents: canvasAgents.map(withLocalA2ARouting),
        edges: agentEdges.map(workflowSpecEdge),
        steps: canvasAgents.map(agent => ({
          id: agent.id,
          kind: "agent",
          uses: agent.id,
          input_keys: agent.input_keys || [],
          output_key: `${agent.id}_result`
        }))
      };
    }

    async function saveCurrentWorkflow() {
      const name = window.prompt("请输入工作流名称", document.getElementById("mode").selectedOptions[0]?.textContent || "我的工作流");
      if (!name || !name.trim()) return;
      const workflow = currentWorkflowSpec(name.trim());
      const data = await post("/api/save-workflow", { repo: document.getElementById("repo").value, workflow });
      if (data.error) {
        alert(data.error);
        return;
      }
      applyLibrary(data.library || { workflows: [data.workflow], agents: [] });
      renderWorkflowModes();
      document.getElementById("mode").value = data.workflow.id;
      current.workflow = data.workflow;
      renderAgents();
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
      const title = node.type === "file" ? escapeAttr(node.path) : "Click to expand";
      return `
        <div class="graph-node ${node.type}" style="left:${node.x}px; top:${node.y}px"
             title="${title}"
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
      const list = document.getElementById("agentPickerList");
      const canvas = document.getElementById("agentCanvas");
      const picker = document.getElementById("agentPicker");
      const mode = document.getElementById("mode").value;
      if (mode === "default" && current.workflow && !canvasAgents.length) {
        canvasAgents = layoutAgents((current.workflow.agents || []).map(withClaudeCode));
        agentEdges = defaultAgentEdges(canvasAgents);
      }
      if (picker) picker.classList.toggle("open", agentPickerOpen);
      if (list) list.innerHTML = availableAgents.map(agent => agentPickerOption(agent)).join("");
      canvas.innerHTML = `<svg class="agent-canvas-svg">${agentEdges.map(edge => agentEdge(edge)).join("")}</svg>` + canvasAgents.map(agent => agentCanvasNode(agent)).join("");
    }

    function layoutAgents(agents) {
      return agents.map((agent, index) => ({ ...withClaudeCode(agent), x: 24 + index * 180, y: 78 + (index % 2) * 28 }));
    }

    function defaultAgentEdges(agents) {
      const edges = [];
      for (let i = 0; i < agents.length - 1; i++) edges.push({ from: agents[i].id, to: agents[i + 1].id });
      return edges;
    }

    function agentPickerOption(agent) {
      const checked = canvasAgents.some(item => item.id === agent.id) ? "checked" : "";
      return `
        <label class="agent-option">
          <input type="checkbox" ${checked} onchange="toggleCanvasAgent('${agent.id}', this.checked)" />
          <span>
            <strong>${escapeHtml(agent.role || agent.id)}</strong>
            <span>${escapeHtml(agent.goal || "")}</span>
          </span>
        </label>
      `;
    }

    function toggleCanvasAgent(id, checked) {
      const exists = canvasAgents.find(agent => agent.id === id);
      if (checked && !exists) {
        const source = availableAgents.find(agent => agent.id === id);
        if (source) canvasAgents.push({ ...withClaudeCode(source), x: 36 + canvasAgents.length * 180, y: 80 + (canvasAgents.length % 2) * 55 });
      }
      if (!checked) {
        canvasAgents = canvasAgents.filter(agent => agent.id !== id);
        agentEdges = agentEdges.filter(edge => edge.from !== id && edge.to !== id);
      }
      renderAgents();
    }

    function agentCanvasNode(agent) {
      const encoded = encodeURIComponent(JSON.stringify(agent));
      const selected = selectedAgentForEdge === agent.id ? " selected" : "";
      return `
        <article class="agent${selected}" style="left:${agent.x || 20}px; top:${agent.y || 20}px"
                 onmousedown="startMoveAgent(event, '${agent.id}')"
                 onclick="selectAgentNode(event, '${agent.id}', '${encoded}')"
                 ondblclick="openAgentEditor(event, '${agent.id}')">
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
      document.getElementById("agentConfig").textContent = JSON.stringify(canvasAgents.find(agent => agent.id === id) || {}, null, 2);
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

    function csvToList(value) {
      return String(value || "").split(",").map(item => item.trim()).filter(Boolean);
    }

    function listToCsv(value) {
      return (value || []).join(", ");
    }

    function setChecked(id, value) {
      document.getElementById(id).checked = Boolean(value);
    }

    function fillAgentWorkbench(agent) {
      const normalized = withClaudeCode(agent);
      const runtime = normalized.runtime || {};
      const permissions = runtime.permissions || {};
      const a2a = normalized.a2a || {};

      document.getElementById("agentWorkbenchId").value = normalized.id || "";
      document.getElementById("agentWorkbenchRole").value = normalized.role || "";
      document.getElementById("agentWorkbenchGoal").value = normalized.goal || "";
      document.getElementById("agentWorkbenchProvider").value = runtime.provider || "";
      document.getElementById("agentWorkbenchModel").value = runtime.model || normalized.model || "";
      document.getElementById("agentWorkbenchSession").value = runtime.session_id || "";
      document.getElementById("agentWorkbenchMcp").value = JSON.stringify(runtime.mcp_servers || [], null, 2);
      document.getElementById("agentWorkbenchSkills").value = listToCsv(runtime.skills || normalized.skills || []);
      document.getElementById("agentWorkbenchTools").value = listToCsv(runtime.tools || []);
      document.getElementById("agentWorkbenchA2AEndpoint").value = a2a.endpoint || `local://agent/${normalized.id}`;
      document.getElementById("agentWorkbenchA2AMessageTypes").value = listToCsv(a2a.message_types || []);
      document.getElementById("agentWorkbenchA2ASubscriptions").value = listToCsv(a2a.subscriptions || []);
      document.getElementById("agentWorkbenchPrompt").value = runtime.system_prompt || normalized.instructions || "";
      setChecked("permReadFiles", permissions.read_files);
      setChecked("permEditFiles", permissions.edit_files);
      setChecked("permRunCommands", permissions.run_commands);
      setChecked("permUseNetwork", permissions.use_network);
      setChecked("permRequiresApproval", permissions.requires_approval);
      document.getElementById("agentEditor").value = JSON.stringify(normalized, null, 2);
      const mcpSection = document.getElementById("agentWorkbenchMcp")?.closest("section");
      const a2aSection = document.getElementById("agentWorkbenchA2AEndpoint")?.closest("section");
      if (mcpSection) mcpSection.style.display = "none";
      if (a2aSection) a2aSection.style.display = "none";
      renderCapabilities(normalized);
    }

    function readAgentWorkbench() {
      const base = JSON.parse(document.getElementById("agentEditor").value);
      const mcpServers = JSON.parse(document.getElementById("agentWorkbenchMcp").value || "[]");
      const skills = csvToList(document.getElementById("agentWorkbenchSkills").value);
      const runtimeTools = csvToList(document.getElementById("agentWorkbenchTools").value);
      const existingAgent = canvasAgents.find(item => item.id === editingAgentId) || {};
      const enabled = existingAgent.runtime?.enabled_capabilities || base.runtime?.enabled_capabilities || [];
      const updated = {
        ...base,
        id: editingAgentId,
        role: document.getElementById("agentWorkbenchRole").value,
        goal: document.getElementById("agentWorkbenchGoal").value,
        instructions: document.getElementById("agentWorkbenchPrompt").value,
        skills,
        model: document.getElementById("agentWorkbenchModel").value || null,
        runtime: {
          ...(base.runtime || {}),
          enabled: true,
          page: "agent_workbench",
          provider: document.getElementById("agentWorkbenchProvider").value || null,
          model: document.getElementById("agentWorkbenchModel").value || null,
          session_id: document.getElementById("agentWorkbenchSession").value || null,
          system_prompt: document.getElementById("agentWorkbenchPrompt").value,
          mcp_servers: mcpServers,
          skills,
          tools: runtimeTools,
          enabled_capabilities: enabled,
          permissions: {
            read_files: document.getElementById("permReadFiles").checked,
            edit_files: document.getElementById("permEditFiles").checked,
            run_commands: document.getElementById("permRunCommands").checked,
            use_network: document.getElementById("permUseNetwork").checked,
            requires_approval: document.getElementById("permRequiresApproval").checked,
          }
        },
        a2a: {
          ...(base.a2a || {}),
          enabled: true,
          endpoint: document.getElementById("agentWorkbenchA2AEndpoint").value || `local://agent/${editingAgentId}`,
          protocol_version: "local-a2a-v1",
          input_modes: ["application/json"],
          output_modes: ["application/json"],
          message_types: csvToList(document.getElementById("agentWorkbenchA2AMessageTypes").value),
          subscriptions: csvToList(document.getElementById("agentWorkbenchA2ASubscriptions").value),
        }
      };
      return applyCapabilitiesLocally(withClaudeCode(updated));
    }

    async function openAgentEditor(event, id) {
      event.stopPropagation();
      editingAgentId = id;
      await loadCapabilities();
      const agent = canvasAgents.find(item => item.id === id);
      if (!agent) return;
      fillAgentWorkbench(agent);
      document.getElementById("agentPage").classList.add("open");
      await refreshCapabilityRecommendations();
    }

    function closeAgentEditor() {
      editingAgentId = null;
      document.getElementById("agentPage").classList.remove("open");
    }

    async function saveAgentEditor() {
      if (!editingAgentId) return;
      try {
        const updated = readAgentWorkbench();
        canvasAgents = canvasAgents.map(agent => agent.id === editingAgentId ? withClaudeCode({ ...agent, ...updated, id: editingAgentId }) : agent);
        availableAgents = mergeAgents(availableAgents, [canvasAgents.find(agent => agent.id === editingAgentId)]);
        const savedAgent = canvasAgents.find(agent => agent.id === editingAgentId);
        const data = await post("/api/save-agent", {
          repo: document.getElementById("repo").value,
          agent: savedAgent,
          capabilities: savedAgent?.runtime?.enabled_capabilities || []
        });
        if (data.error) {
          alert(data.error);
          return;
        }
        applyLibrary(data.library || { workflows: savedWorkflows, agents: [data.agent] });
        document.getElementById("agentConfig").textContent = JSON.stringify(canvasAgents.find(agent => agent.id === editingAgentId), null, 2);
        closeAgentEditor();
        renderAgents();
      } catch (error) {
        alert(`JSON 格式错误：${error}`);
      }
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

    renderWorkflowModes();
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
